//! Replay an AI decision log (JSONL) and show what the **current** sanity
//! pipeline does to each entry's ranking. For every log line the tool:
//!
//! 1. Parses the entry (snapshot, intent, plan pool with raw factors).
//! 2. Rebuilds `InfluenceMaps` deterministically from the snapshot.
//! 3. Feeds the logged raw factors through the live `finalize_scores` so
//!    scores match production bit-for-bit (summon_bonus, trade_bonus,
//!    hash-based noise, batch normalisation).
//! 4. Runs `sanity_adjust_plans` on that score vector.
//! 5. Picks the post-sanity winner via the live `pick_best_plan` (mercy
//!    + top-K RNG tiebreak). Pre-sanity top uses argmax as a diagnostic
//!      reference.
//! 6. Prints the original top plan and the post-sanity top plan side-by-side,
//!    flagging entries where the choice changed.
//!
//! Usage: `cargo run --bin replay_ai_log -- logs/<file>.jsonl [--verbose]`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use bevy::prelude::Entity;
use serde::Deserialize;

use storyforge::combat::ai::difficulty::DifficultyProfile;
use storyforge::combat::ai::factors::PlanFactors;
use storyforge::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use storyforge::combat::ai::planning::{
    apply_protect_self_mask, finalize_scores, pick_best_plan, sanity_adjust_plans, PlanStep,
    StepOutcome, TurnPlan,
};
use storyforge::combat::ai::role::AxisProfile;
use storyforge::combat::ai::snapshot::BattleSnapshot;
use storyforge::combat::ai::reservations::Reservations;
use storyforge::combat::ai::utility::{AiWorld, ScoringCtx};
use storyforge::content::content_view::ContentView;
use storyforge::core::DiceRng;
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
    // reason_text is present in the log schema but unused here; serde
    // tolerates it via #[serde(default)] on a dropped field.
    #[serde(default, rename = "reason_text")]
    _reason_text: String,
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
    /// `None` when the game pruned the plan before scoring (e.g. beam-search
    /// early rejection). Such plans still appear in the log so we can see
    /// what was considered, but they have no numeric score to compare
    /// against. Replay treats them as NEG_INFINITY — identical to a plan
    /// masked by sanity — so `argmax` naturally ignores them.
    score: Option<f32>,
    /// v6+: pre-adaptation score. Older logs default to `None` (no
    /// adaptation concept existed). Reserved for future `--show-adapt`
    /// diff mode; currently the replayer recomputes its own base via
    /// `raw_factors` so the logged number isn't used in rendering, but
    /// it's kept on `PlanLog` so offline analyzers can round-trip it.
    #[serde(default)]
    #[allow(dead_code)]
    base_score: Option<f32>,
    /// v6+: which evaluation regime scored this plan. Older logs default
    /// to `Default`.
    #[serde(default)]
    evaluation_mode: LoggedEvaluationMode,
    /// v6+: fact that triggered a mode switch; `None` when
    /// `evaluation_mode = Default`.
    #[serde(default)]
    adaptation_reason: Option<LoggedAdaptationReason>,
    /// v7+: per-plan trade breakdown + post-tanh score contribution.
    /// Older logs default to an all-zero block — render suppresses the
    /// line when `delta == 0 && !self_lethal`.
    #[serde(default)]
    trade: LoggedTradeBlock,
}

/// Mirrors `log::TradeBlock`. Verbose-only rendering; not consumed by
/// the scoring reconstruction.
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[allow(dead_code)]
struct LoggedTradeBlock {
    #[serde(default)]
    delta: f32,
    #[serde(default)]
    killed: f32,
    #[serde(default)]
    lost: f32,
    #[serde(default)]
    self_lost: f32,
    #[serde(default)]
    self_lethal: bool,
    #[serde(default)]
    score: f32,
}

/// Mirrors `planning::adaptation::EvaluationMode` for deserialization.
/// Keep in sync with the enum's serde rename when variants change.
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LoggedEvaluationMode {
    #[default]
    Default,
    LastStand,
}

impl LoggedEvaluationMode {
    fn is_adapted(self) -> bool {
        !matches!(self, LoggedEvaluationMode::Default)
    }
}

/// Mirrors `planning::adaptation::AdaptationReason` for deserialization.
/// We don't need the numeric payloads during replay — just the kind —
/// so the variants are kept unit and tagged like the game enum.
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LoggedAdaptationReason {
    ExpectedSelfLethal {
        #[serde(default)]
        #[allow(dead_code)]
        aoo_dmg: f32,
        #[serde(default)]
        #[allow(dead_code)]
        actor_hp: i32,
    },
    ProtectSelfNoDefensive,
}

impl LoggedAdaptationReason {
    fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
        }
    }
}

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
    let mut rng = DiceRng::with_seed(0);

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
        // Older logs lack newer per-snapshot fields; `#[serde(default)]` on
        // each addition fills them with neutral defaults so replay
        // continues, just blind to those signals:
        // - v1 → v2: `reactions_left` (1) + `aoo_expected_damage` (None)
        // - v2 → v3: `caster_ctx` (zeros) + `crit_fail_effect` (Miss)
        // - v3 → v4: `damage_horizon` (empty) — CC/heal fall back to threat
        // - v4 → v5: `intent.reason` — structured reason payload; replay does
        //   not read it (classification still uses `selection_kind`).
        // - v5 → v6: per-plan ADAPTATION dump. Replay surfaces it in verbose
        //   output; older logs default to `evaluation_mode=default` and
        //   `adaptation_reason=None` so the renderer stays silent.
        // - v6 → v7: per-plan TRADE block (delta/killed/lost/self_lost/
        //   self_lethal/score). Replay surfaces the breakdown under
        //   `--verbose`; older logs drop to a default-filled block.
        if !(1..=7).contains(&entry.schema_version) {
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

        let maps = build_influence_maps(&entry.snapshot, actor, active.team, &inf_cfg);

        let world = AiWorld {
            content: &content,
            difficulty: &difficulty,
            crit_fail_chance: 0.0,
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
                sim_snapshots: Vec::new(),
            })
            .collect();
        // Convert logged raw factor arrays back to structured PlanFactors so
        // the shared scoring pipeline can ingest them directly.
        let raw_factors: Vec<PlanFactors> = entry
            .plans
            .iter()
            .map(|p| PlanFactors::from_array(p.raw_factors))
            .collect();

        // Reservations are empty during replay — each entry is scored in
        // isolation, without the round's coordination state from live play.
        let reservations = Reservations::default();
        let scoring_ctx = ScoringCtx {
            world: &world,
            maps: &maps,
            reservations: &reservations,
            snap: &entry.snapshot,
            active: &active,
        };

        // Reuse the production `finalize_scores` so summon_bonus, trade_bonus,
        // hash-based noise, and batch normalisation all match the live
        // pipeline bit-for-bit. Invariant: replay's pre-sanity score equals
        // what production produced given the same raw factors.
        let mut scores = finalize_scores(&plans, &raw_factors, &scoring_ctx);
        let pre_scores = scores.clone();

        sanity_adjust_plans(&mut scores, &plans, &scoring_ctx);

        // ProtectSelf mask — two paths:
        //   1. The logged intent is already ProtectSelf (fix A deployed at
        //      log time, or it was a hard panic override). Apply B directly.
        //   2. `--simulate-ab` + logged intent was a viability fallback AND
        //      midpanic conditions now hold → simulate the switch. Raw
        //      factors stay as-logged (they were computed under the old
        //      intent), so this under-counts ProtectSelf's intent-factor
        //      boost on defensive plans. Enough for directional verification.
        // MVP1: replay does not reconstruct ADAPTATION yet (Phase 7 extends
        // schema to v6 and pipes adaptation.modes through). For now pass a
        // default-mode vector so every plan participates in the contract
        // mask as before — preserves replay semantics on v1-v5 logs.
        let modes = vec![
            storyforge::combat::ai::planning::EvaluationMode::Default;
            plans.len()
        ];
        let mut applied_mask = false;
        let mut simulated_switch = false;
        if matches!(
            entry.intent.intent,
            storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
        ) {
            let margin = difficulty.defensive_tile_margin();
            apply_protect_self_mask(&mut scores, &plans, &modes, &active, &content, &maps, margin);
            applied_mask = true;
        } else if simulate_ab && entry.intent.selection_kind == "viability_fallback" {
            let hp_pct = active.hp_pct();
            let actor_danger = maps.danger.get(active.pos);
            let midpanic_hp = difficulty.midpanic_hp_threshold();
            let panic_danger = difficulty.awareness_danger_threshold();
            if hp_pct < midpanic_hp && actor_danger > panic_danger {
                let margin = difficulty.defensive_tile_margin();
                apply_protect_self_mask(&mut scores, &plans, &modes, &active, &content, &maps, margin);
                applied_mask = true;
                simulated_switch = true;
            }
        }
        let _ = applied_mask;

        // Compare rankings. Pre-sanity uses argmax as a simple reference
        // point ("what a perfect-information picker would take").
        // Post-sanity goes through the production `pick_best_plan` so
        // replay's final pick reflects mercy reordering and top-K
        // tie-breaking exactly as the live pipeline would. Replay's rng
        // is seeded independently of production's live state, so tie-breaks
        // on normal/easy difficulty (where top_k > 1 and multiple plans
        // fall within `window`) may diverge — that's RNG drift, not a
        // logic mismatch.
        let top_pre = argmax(&pre_scores);
        let (top_post, _pick_mech) = pick_best_plan(&scores, &raw_factors, &world, &mut rng);

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
                // Surface per-plan ADAPTATION metadata (v6+). Older logs
                // default to Default/None → tag stays empty.
                let adapt_tag = if entry.plans[i].evaluation_mode.is_adapted() {
                    match &entry.plans[i].adaptation_reason {
                        Some(r) => format!("  [adapted: last_stand ← {}]", r.code()),
                        None => "  [adapted: last_stand]".to_string(),
                    }
                } else {
                    String::new()
                };
                // v7 trade block. Quiet when the plan didn't make a trade
                // — no kills, no ally losses, no self-lethal exposure.
                let trade = &entry.plans[i].trade;
                let trade_tag = if trade.delta != 0.0
                    || trade.self_lethal
                    || trade.killed != 0.0
                    || trade.lost != 0.0
                {
                    let self_tag = if trade.self_lethal { " SELF-LETHAL" } else { "" };
                    format!(
                        "  [trade: Δ={:+.1} (kill {:+.1} / lost {:+.1} / self {:+.1}) score={:+.2}{}]",
                        trade.delta, trade.killed, trade.lost, trade.self_lost,
                        trade.score, self_tag,
                    )
                } else {
                    String::new()
                };
                println!(
                    "      #{}{}  pre={:+.2}  post={:+.2}  Δ={:+.2}  final=({},{})  {}{}{}",
                    entry.plans[i].rank,
                    if entry.plans[i].chosen { "★" } else { " " },
                    pre,
                    post,
                    post - pre,
                    entry.plans[i].final_pos[0],
                    entry.plans[i].final_pos[1],
                    plan_shape(&entry.plans[i]),
                    adapt_tag,
                    trade_tag,
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
                     // `None` = plan was pruned pre-scoring in the live run.
}

/// Silences dead_code lints on `AxisProfile::factor_weights` when only
/// referenced via deser chain.
#[allow(dead_code)]
fn _touch_axis(p: &AxisProfile) -> [f32; 9] {
    p.factor_weights()
}
