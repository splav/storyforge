//! Replay an AI decision log (JSONL) and show what the **current** sanity
//! pipeline does to each entry's ranking. For every log line the tool:
//!
//! 1. Parses the entry (snapshot, intent, plan pool with raw factors).
//! 2. Rebuilds `InfluenceMaps` deterministically from the snapshot.
//! 3. Re-normalizes raw factors and applies role weights to reproduce the
//!    "pre-sanity" score the game computed at logging time (minus noise).
//! 4. Runs `sanity_adjust_plans` on that score vector.
//! 5. Prints the original top plan and the post-sanity top plan side-by-side,
//!    flagging entries where the choice changed.
//!
//! Usage: `cargo run --bin replay_ai_log -- logs/<file>.jsonl [--verbose]`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use bevy::prelude::Entity;
use serde::Deserialize;

use storyforge::combat::ai::difficulty::DifficultyProfile;
use storyforge::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use storyforge::combat::ai::planning::{
    apply_protect_self_mask, sanity_adjust_plans, PlanStep, StepOutcome, TurnPlan,
};
use storyforge::combat::ai::reservations::Reservations;
use storyforge::combat::ai::role::AxisProfile;
use storyforge::combat::ai::snapshot::BattleSnapshot;
use storyforge::combat::ai::utility::{ActorCtx, AiWorld, UtilityContext};
use storyforge::content::abilities::CasterContext;
use storyforge::content::content_view::ContentView;
use storyforge::content::races::CritFailEffect;
use storyforge::core::DiceRng;
use storyforge::game::components::Abilities;
use storyforge::game::hex::Hex;

/// Mirror of `log::AiLogEntry` with owned fields so we can deserialize.
#[derive(Deserialize)]
struct LogEntry {
    schema_version: u32,
    #[allow(dead_code)]
    plan_id: u64,
    #[allow(dead_code)]
    timestamp_ms: u128,
    decision_time_ms: u64,
    round: u32,
    actor_id: u64,
    actor_name: String,
    actor_ap: i32,
    actor_max_ap: i32,
    actor_mp: i32,
    #[allow(dead_code)]
    actor_max_mp: i32,
    plans_evaluated: usize,
    #[allow(dead_code)]
    plans_shown: usize,
    snapshot: BattleSnapshot,
    intent: IntentBlock,
    plans: Vec<PlanLog>,
    committed_decision: serde_json::Value,
}

#[derive(Deserialize)]
struct IntentBlock {
    intent: storyforge::combat::ai::intent::TacticalIntent,
    selection_kind: String,
    reason_text: String,
}

#[derive(Deserialize)]
struct PlanLog {
    rank: usize,
    chosen: bool,
    steps: Vec<PlanStep>,
    outcomes: Vec<StepOutcome>,
    final_pos: [i32; 2],
    residual_ap: i32,
    residual_mp: i32,
    raw_factors: [f32; 9],
    score: f32,
}

const SIGNED_FACTOR: [bool; 9] = [
    false, false, false, false, true, false, false, true, true,
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut verbose = false;
    let mut simulate_ab = false;
    let mut path: Option<PathBuf> = None;
    for a in &args[1..] {
        if a == "--verbose" || a == "-v" {
            verbose = true;
        } else if a == "--simulate-ab" {
            simulate_ab = true;
        } else if !a.starts_with('-') {
            path = Some(PathBuf::from(a));
        }
    }
    let Some(path) = path else {
        eprintln!("usage: replay_ai_log <log.jsonl> [--verbose]");
        std::process::exit(2);
    };

    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let reader = BufReader::new(file);

    let content = ContentView::load_global_for_tests();
    let inf_cfg = InfluenceConfig::default();
    let difficulty = DifficultyProfile::normal();
    let reservations = Reservations::default();
    let mut rng = DiceRng::with_seed(0);
    let _ = &mut rng;

    let mut total = 0usize;
    let mut changed = 0usize;

    println!("\n=== Replay: {} ===\n", path.display());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: LogEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };
        // v1 logs lack `reactions_left` / `aoo_expected_damage` on UnitSnapshot
        // — `#[serde(default)]` fills them (1 and None respectively). AoO
        // penalty on v1 logs will be blind to damage magnitude but still sees
        // the adjacency transitions. v2 carries full data.
        if entry.schema_version < 1 || entry.schema_version > 2 {
            eprintln!("unsupported schema_version {}, skipping", entry.schema_version);
            continue;
        }
        total += 1;

        // Rebuild context.
        let actor = match Entity::try_from_bits(entry.actor_id) {
            Some(e) => e,
            None => {
                eprintln!("invalid actor_id {}, skipping", entry.actor_id);
                continue;
            }
        };
        let Some(active) = entry.snapshot.unit(actor).cloned() else {
            eprintln!("actor not found in snapshot, skipping");
            continue;
        };

        let maps = build_influence_maps(&entry.snapshot, active.team, &inf_cfg);

        let caster = CasterContext {
            str_mod: 0,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let abilities = Abilities(active.abilities.clone());
        let ctx = UtilityContext {
            world: AiWorld {
                content: &content,
                difficulty: &difficulty,
            },
            actor: ActorCtx {
                caster: &caster,
                abilities: &abilities,
                crit_fail_effect: CritFailEffect::Miss,
                crit_fail_chance: 0.0,
            },
        };

        // Reconstruct TurnPlan[] from log + raw factor matrix.
        let plans: Vec<TurnPlan> = entry
            .plans
            .iter()
            .map(|p| TurnPlan {
                steps: p.steps.clone(),
                final_pos: Hex::new(p.final_pos[0], p.final_pos[1]),
                residual_ap: p.residual_ap,
                residual_mp: p.residual_mp,
                outcomes: p.outcomes.clone(),
                partial_score: 0.0,
            })
            .collect();
        let raw: Vec<[f32; 9]> = entry.plans.iter().map(|p| p.raw_factors).collect();

        // Re-normalize raw factors batch-relative (matches score_plans_with_raw).
        let mut maxes = [0.0f32; 9];
        let mut mins = [0.0f32; 9];
        for row in &raw {
            for (i, &v) in row.iter().enumerate() {
                if v > maxes[i] { maxes[i] = v; }
                if v < mins[i] { mins[i] = v; }
            }
        }
        let denom: [f32; 9] = std::array::from_fn(|i| {
            if SIGNED_FACTOR[i] { mins[i].abs().max(maxes[i].abs()) } else { maxes[i] }
        });

        let mut weights = active.role.factor_weights();
        weights[7] *= difficulty.intent_commitment;
        weights[8] *= difficulty.resource_discipline;

        // Noise is skipped — replay aims to be deterministic. Logged `score`
        // may differ by |noise| ≤ score_noise (0 on hard, ~0.15 on normal).
        let mut scores: Vec<f32> = raw
            .iter()
            .map(|row| {
                (0..9)
                    .map(|i| {
                        let n = if denom[i] > f32::EPSILON { row[i] / denom[i] } else { 0.0 };
                        n * weights[i]
                    })
                    .sum()
            })
            .collect();
        let pre_scores = scores.clone();

        // Apply current sanity_adjust.
        sanity_adjust_plans(&mut scores, &plans, &active, &entry.snapshot, &maps, &ctx);

        // ProtectSelf mask — two paths:
        //   1. The logged intent is already ProtectSelf (fix A deployed at
        //      log time, or it was a hard panic override). Apply B directly.
        //   2. `--simulate-ab` + logged intent was a viability fallback AND
        //      midpanic conditions now hold → simulate the switch. Raw
        //      factors stay as-logged (they were computed under the old
        //      intent), so this under-counts ProtectSelf's intent-factor
        //      boost on defensive plans. Enough for directional verification.
        let mut applied_mask = false;
        let mut simulated_switch = false;
        if matches!(
            entry.intent.intent,
            storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
        ) {
            let margin = difficulty.defensive_tile_margin();
            apply_protect_self_mask(&mut scores, &plans, &active, &content, &maps, margin);
            applied_mask = true;
        } else if simulate_ab && entry.intent.selection_kind == "viability_fallback" {
            let hp_pct = active.hp_pct();
            let actor_danger = maps.danger.get(active.pos);
            let midpanic_hp = difficulty.midpanic_hp_threshold();
            let panic_danger = difficulty.awareness_danger_threshold();
            if hp_pct < midpanic_hp && actor_danger > panic_danger {
                let margin = difficulty.defensive_tile_margin();
                apply_protect_self_mask(&mut scores, &plans, &active, &content, &maps, margin);
                applied_mask = true;
                simulated_switch = true;
            }
        }
        let _ = applied_mask;

        // Compare rankings.
        let top_pre = argmax(&pre_scores);
        let top_post = argmax(&scores);

        let pre_was_chosen = entry.plans.iter().find(|p| p.chosen).map(|p| p.rank).unwrap_or(0);
        let hp = format!("{}/{}", active.hp, active.max_hp);

        let header = format!(
            "r{} {}: HP {} AP {}/{} MP {}, intent={} [{}], plans_eval={}, decision={}ms",
            entry.round,
            entry.actor_name,
            hp,
            entry.actor_ap,
            entry.actor_max_ap,
            entry.actor_mp,
            intent_kind(&entry.intent.intent),
            entry.intent.selection_kind,
            entry.plans_evaluated,
            entry.decision_time_ms,
        );

        if top_pre != top_post {
            changed += 1;
            let sim_tag = if simulated_switch { " (simulated A+B midpanic)" } else { "" };
            println!("🔁 {header}{sim_tag}", header = header);
            println!("   logged_chose=#{pre_was_chosen}, pre_sanity_top=#{} ({:+.2}), post_sanity_top=#{} ({:+.2})",
                top_pre + 1, pre_scores[top_pre], top_post + 1, scores[top_post]);
            print_plan("   pre ", &entry.plans[top_pre], pre_scores[top_pre], scores[top_pre]);
            print_plan("   post", &entry.plans[top_post], pre_scores[top_post], scores[top_post]);
            let _ = entry.committed_decision;
        } else if verbose {
            println!("=  {header}");
            println!("   logged_chose=#{pre_was_chosen}, top=#{} ({:+.2} → {:+.2})",
                top_pre + 1, pre_scores[top_pre], scores[top_pre]);
        }

        if verbose {
            println!("   — full ranking (pre → post) —");
            let mut indexed: Vec<(usize, f32, f32)> = (0..scores.len())
                .map(|i| (i, pre_scores[i], scores[i]))
                .collect();
            indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (i, pre, post) in indexed {
                println!(
                    "      #{}{}  pre={:+.2}  post={:+.2}  Δ={:+.2}  final=({},{})  {}",
                    entry.plans[i].rank,
                    if entry.plans[i].chosen { "★" } else { " " },
                    pre,
                    post,
                    post - pre,
                    entry.plans[i].final_pos[0],
                    entry.plans[i].final_pos[1],
                    plan_shape(&entry.plans[i]),
                );
            }
        }
    }

    println!("\n=== {} entries, {} ranking changes after sanity ===", total, changed);
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    let mut best_score = f32::NEG_INFINITY;
    for (i, &s) in v.iter().enumerate() {
        if s.is_finite() && s > best_score {
            best_score = s;
            best = i;
        }
    }
    best
}

fn intent_kind(i: &storyforge::combat::ai::intent::TacticalIntent) -> &'static str {
    use storyforge::combat::ai::intent::TacticalIntent::*;
    match i {
        FocusTarget { .. } => "FocusTarget",
        ApplyCC { .. } => "ApplyCC",
        Reposition => "Reposition",
        ProtectSelf => "ProtectSelf",
        ProtectAlly { .. } => "ProtectAlly",
        SetupAOE => "SetupAOE",
        LastStand => "LastStand",
    }
}

fn plan_shape(p: &PlanLog) -> String {
    let mut out = Vec::new();
    for s in &p.steps {
        match s {
            PlanStep::Move { path } => {
                let last = path.last().copied().unwrap_or(Hex::ZERO);
                out.push(format!("Move→({},{})", last.x, last.y));
            }
            PlanStep::Cast { ability, target, .. } => {
                out.push(format!("Cast({}→{})", ability.0, target.to_bits()));
            }
        }
    }
    out.join(" · ")
}

fn print_plan(label: &str, p: &PlanLog, pre: f32, post: f32) {
    println!(
        "{label} #{} score {:+.2}→{:+.2}  {}  raw={:?}",
        p.rank,
        pre,
        post,
        plan_shape(p),
        p.raw_factors,
    );
    let _ = p.score; // logged score includes noise; we show recomputed.
}

/// Silences dead_code lints on `AxisProfile::factor_weights` when only
/// referenced via deser chain.
#[allow(dead_code)]
fn _touch_axis(p: &AxisProfile) -> [f32; 9] {
    p.factor_weights()
}
