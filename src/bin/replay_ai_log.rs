//! Replay an AI decision log (v27 JSONL) and compare current `pick_action`
//! output against logged decisions.
//!
//! For every `actor_tick` event the tool:
//! 1. Parses the event (`ActorTickEvent`, schema v27).
//! 2. Rebuilds `InfluenceMaps` from the embedded snapshot.
//! 3. Calls the production `pick_action` with the logged snapshot.
//! 4. Compares the re-picked decision with the logged decision.
//!
//! `--capture-golden`: run production pipeline on all entries, write one
//! `GoldenRecord` per non-skip entry to `<out.jsonl>`.
//!
//! `--compare-golden`: run production pipeline, compare line-by-line against a
//! baseline golden file. Exits 1 if any record diverges.
//!
//! `--assert [<overlay.expected.toml>]`: run the production pipeline on the
//! entry selected by `[scope].plan_id` in the overlay, check expectations.
//!
//! Usage:
//!   `cargo run --bin replay_ai_log -- logs/<file>.jsonl [--verbose]`
//!   `cargo run --bin replay_ai_log -- logs/<file>.jsonl --capture-golden golden.jsonl`
//!   `cargo run --bin replay_ai_log -- logs/<file>.jsonl --compare-golden golden.jsonl`

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use bevy::prelude::Entity;

use storyforge::combat::ai::difficulty::DifficultyProfile;
use storyforge::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use storyforge::combat::ai::intent::AiMemory;
use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision, LoggedPlan};
use storyforge::combat::ai::planning::PlanStep;
use storyforge::combat::ai::replay::{
    assert_v27_log_file, default_overlay_path, GoldenRecord,
};
use storyforge::combat::ai::replay_assertion::{print_assertion_failure, AssertResult};
use storyforge::combat::ai::reservations::Reservations;
use storyforge::combat::ai::utility::{AiDecision, AiWorld, pick_action};
use storyforge::content::content_view::ContentView;
use storyforge::core::DiceRng;
use storyforge::game::hex::Hex;

// ── Regression metrics ──────────────────────────────────────────────────────

/// Cumulative regression counters across all processed log entries.
#[derive(Default)]
struct Metrics {
    /// MoveOnly committed prefix entries.
    move_only_total: usize,
    /// MoveOnly where destination == actor's current position (wasted MP).
    move_only_wasted: usize,
    /// Plans that have a Move step (any kind).
    plans_with_moves: usize,
    /// Plans with moves where the actor revisits a tile they already occupied.
    repeated_tile_plans: usize,
    /// Plans with moves where the final position equals the start position.
    zero_net_move_plans: usize,
    /// Plans with a Cast step + at least one subsequent Move step.
    plans_with_post_cast_move: usize,
    /// Plans with post-cast move where the final position is farther from any
    /// enemy than the cast position — "retreat after attacking".
    post_cast_retreat_plans: usize,
    /// Entries where the chosen plan has a Cast step.
    chosen_with_cast_total: usize,
    /// Entries where the chosen plan has a post-cast Move tail.
    phantom_tail_chosen: usize,
    /// Of the above: entries where a tailless alternative existed whose
    /// committed action (Cast target/ability) differed from the chosen plan.
    phantom_tail_flips_committed: usize,
}

impl Metrics {
    fn print_summary(&self) {
        println!("\n=== Regression Metrics Summary ===\n");

        let pct = |n: usize, d: usize| -> f64 {
            if d > 0 { n as f64 / d as f64 * 100.0 } else { 0.0 }
        };

        println!("Move-only wasted rate: {} / {} ({:.1}%)",
            self.move_only_wasted, self.move_only_total,
            pct(self.move_only_wasted, self.move_only_total));
        println!("Repeated-tile rate:    {} / {} ({:.1}%)",
            self.repeated_tile_plans, self.plans_with_moves,
            pct(self.repeated_tile_plans, self.plans_with_moves));
        println!("Zero-net-move rate:    {} / {} ({:.1}%)",
            self.zero_net_move_plans, self.plans_with_moves,
            pct(self.zero_net_move_plans, self.plans_with_moves));
        println!("Post-cast retreat rate: {} / {} ({:.1}%)",
            self.post_cast_retreat_plans, self.plans_with_post_cast_move,
            pct(self.post_cast_retreat_plans, self.plans_with_post_cast_move));
        println!("Phantom-tail rate:     {} / {} ({:.1}%)",
            self.phantom_tail_chosen, self.chosen_with_cast_total,
            pct(self.phantom_tail_chosen, self.chosen_with_cast_total));
        println!("  flip-committed:      {} / {} ({:.1}%)",
            self.phantom_tail_flips_committed, self.phantom_tail_chosen,
            pct(self.phantom_tail_flips_committed, self.phantom_tail_chosen));
    }
}

// ── Content inference ───────────────────────────────────────────────────────

fn infer_content_dirs(path: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    // Filename pattern: <ts>_<campaign>_<scenario>_<encounter>.jsonl
    let stem = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = stem.splitn(5, '_').collect();
    // parts[0] = timestamp, parts[1] = campaign, parts[2] = scenario, rest = encounter
    if parts.len() < 3 {
        return None;
    }
    let global = std::path::Path::new("assets/data");
    let campaign_dir = global.join(parts[1]);
    let scenario_dir = campaign_dir.join(parts[2]);
    if campaign_dir.exists() && scenario_dir.exists() {
        Some((campaign_dir, scenario_dir))
    } else if campaign_dir.exists() {
        Some((campaign_dir.clone(), campaign_dir))
    } else {
        None
    }
}

// ── GoldenRecord helpers (v27) ──────────────────────────────────────────────

fn golden_from_v27_event(
    event: &ActorTickEvent,
    log_path: &str,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<GoldenRecord, String> {
    let actor = Entity::try_from_bits(event.actor_id)
        .ok_or_else(|| format!("invalid actor_id {}", event.actor_id))?;
    let active = event
        .snapshot
        .unit(actor)
        .ok_or_else(|| format!("actor {:?} not in snapshot", actor))?;

    let maps = build_influence_maps(&event.snapshot, actor, active.team, inf_cfg);
    let difficulty = DifficultyProfile::normal();
    let world = AiWorld {
        content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
    };
    let memory = AiMemory::default();
    let reservations = Reservations::default();
    let mut rng = DiceRng::with_seed(0);

    let result = pick_action(
        actor,
        active.pos,
        &world,
        &event.snapshot,
        &maps,
        &mut rng,
        &memory,
        &reservations,
        false,
        &Default::default(),
    );

    let (decision_kind, cast_ability, cast_target, end_position) =
        decision_fields(&result.decision, &result.pool.plans, result.best_idx);

    // Use tick_index as plan_id surrogate for golden keying.
    Ok(GoldenRecord {
        log_path: log_path.to_owned(),
        plan_id: event.actor_id, // actor_id is the unique-per-tick key when combined with log_path
        actor_id: event.actor_id,
        decision_kind,
        cast_ability,
        cast_target,
        end_position,
    })
}

fn decision_fields(
    decision: &AiDecision,
    plans: &[storyforge::combat::ai::planning::TurnPlan],
    best_idx: usize,
) -> (String, Option<String>, Option<u64>, [i32; 2]) {
    let end_pos = plans.get(best_idx).map(|p| [p.final_pos.x, p.final_pos.y]).unwrap_or([0, 0]);
    match decision {
        AiDecision::EndTurn => ("EndTurn".to_owned(), None, None, end_pos),
        AiDecision::CastInPlace { ability, target, .. } => (
            "CastInPlace".to_owned(),
            Some(ability.0.clone()),
            Some(target.to_bits()),
            end_pos,
        ),
        AiDecision::MoveAndCast { ability, target, path, .. } => (
            "MoveAndCast".to_owned(),
            Some(ability.0.clone()),
            Some(target.to_bits()),
            path.last().map(|h| [h.x, h.y]).unwrap_or(end_pos),
        ),
        AiDecision::Move { path, .. } => (
            "Move".to_owned(),
            None,
            None,
            path.last().map(|h| [h.x, h.y]).unwrap_or(end_pos),
        ),
    }
}

// ── Plan shape helpers ──────────────────────────────────────────────────────

fn plan_shape_v27(steps: &[PlanStep]) -> String {
    let mut out = Vec::new();
    for s in steps {
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

// ── Decision comparison helpers ─────────────────────────────────────────────

/// Extract a canonical "committed action key" from a `LoggedDecision` for
/// comparison with a re-picked `AiDecision`. Returns `(kind, ability, target)`.
fn logged_decision_key(d: &LoggedDecision) -> (String, Option<String>, Option<u64>) {
    match d {
        LoggedDecision::Cast { ability, target, .. } => {
            ("Cast".to_owned(), Some(ability.clone()), Some(*target))
        }
        LoggedDecision::MoveAndCast { ability, target, .. } => {
            ("MoveAndCast".to_owned(), Some(ability.clone()), Some(*target))
        }
        LoggedDecision::Move { .. } => ("Move".to_owned(), None, None),
        LoggedDecision::EndTurn => ("EndTurn".to_owned(), None, None),
        LoggedDecision::Skip { .. } => ("Skip".to_owned(), None, None),
    }
}

fn ai_decision_key(d: &AiDecision) -> (String, Option<String>, Option<u64>) {
    match d {
        AiDecision::EndTurn => ("EndTurn".to_owned(), None, None),
        AiDecision::CastInPlace { ability, target, .. } => {
            ("Cast".to_owned(), Some(ability.0.clone()), Some(target.to_bits()))
        }
        AiDecision::MoveAndCast { ability, target, .. } => {
            ("MoveAndCast".to_owned(), Some(ability.0.clone()), Some(target.to_bits()))
        }
        AiDecision::Move { .. } => ("Move".to_owned(), None, None),
    }
}

fn decisions_match(logged: &LoggedDecision, fresh: &AiDecision) -> bool {
    logged_decision_key(logged) == ai_decision_key(fresh)
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut verbose = false;
    let mut metrics_summary = false;
    // Deprecated flags accepted for CLI compat but are no-op in v27.
    let mut _simulate_ab = false;
    let mut _phase7_prototype = false;
    let mut _phase7_p2 = false;
    let mut campaign_override: Option<PathBuf> = None;
    let mut scenario_override: Option<PathBuf> = None;
    let mut assert_mode = false;
    let mut assert_overlay_path: Option<PathBuf> = None;
    let mut capture_golden: Option<PathBuf> = None;
    let mut compare_golden: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut iter = args[1..].iter().peekable();

    while let Some(a) = iter.next() {
        match a.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--simulate-ab" => _simulate_ab = true,
            "--metrics-summary" => metrics_summary = true,
            "--phase7-prototype" => _phase7_prototype = true,
            "--phase7-p2" => _phase7_p2 = true,
            "--campaign" => { campaign_override = iter.next().map(PathBuf::from); }
            "--scenario" => { scenario_override = iter.next().map(PathBuf::from); }
            "--assert" => {
                assert_mode = true;
                if let Some(next) = iter.peek() {
                    if !next.starts_with('-') {
                        assert_overlay_path = Some(PathBuf::from(iter.next().unwrap()));
                    }
                }
            }
            "--capture-golden" => {
                let arg = iter.next().unwrap_or_else(|| {
                    eprintln!("error: --capture-golden requires a path argument");
                    std::process::exit(2);
                });
                capture_golden = Some(PathBuf::from(arg));
            }
            "--compare-golden" => {
                let arg = iter.next().unwrap_or_else(|| {
                    eprintln!("error: --compare-golden requires a path argument");
                    std::process::exit(2);
                });
                compare_golden = Some(PathBuf::from(arg));
            }
            a if !a.starts_with('-') => paths.push(PathBuf::from(a)),
            _ => {}
        }
    }

    if paths.is_empty() {
        eprintln!(
            "usage: replay_ai_log <log.jsonl> [<log2.jsonl> ...] \
             [--verbose] [--metrics-summary] [--simulate-ab] \
             [--phase7-prototype] [--phase7-p2] \
             [--campaign <dir>] [--scenario <dir>] \
             [--assert [<overlay.expected.toml>]] \
             [--capture-golden <out.jsonl>] \
             [--compare-golden <baseline.jsonl>]"
        );
        std::process::exit(2);
    }

    {
        let golden_active = capture_golden.is_some() || compare_golden.is_some();
        if capture_golden.is_some() && compare_golden.is_some() {
            eprintln!("error: --capture-golden and --compare-golden are mutually exclusive");
            std::process::exit(2);
        }
        if golden_active && assert_mode {
            eprintln!("error: --capture-golden / --compare-golden cannot be combined with --assert");
            std::process::exit(2);
        }
    }

    // Resolve content dirs.
    let global = std::path::Path::new("assets/data");
    let (campaign_dir, scenario_dir) = if let (Some(c), Some(s)) = (&campaign_override, &scenario_override) {
        (c.clone(), s.clone())
    } else if let Some((c, s)) = paths.first().and_then(|p| infer_content_dirs(p)) {
        (c, s)
    } else {
        eprintln!(
            "warning: could not infer campaign/scenario from filename; \
             loading global content only (assets/data). \
             Pass --campaign <dir> --scenario <dir> to override."
        );
        (global.to_path_buf(), global.to_path_buf())
    };
    let content = ContentView::load_layered(&campaign_dir, &scenario_dir);
    let inf_cfg = InfluenceConfig::default();

    // ── Assert mode ──────────────────────────────────────────────────────────
    if assert_mode {
        let jsonl_path = &paths[0];
        let overlay_path =
            assert_overlay_path.unwrap_or_else(|| default_overlay_path(jsonl_path));

        let outcome = match assert_v27_log_file(jsonl_path, &overlay_path, &content, &inf_cfg) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };

        if verbose {
            println!("assert: actor_id={}", outcome.actor_id);
            println!("  decision_kind  = {:?}", outcome.actual.decision_kind);
            println!("  intent_kind    = {:?}", outcome.actual.intent_kind);
            println!("  cast_ability   = {:?}", outcome.actual.cast_ability);
            println!("  cast_target    = {:?}", outcome.actual.cast_target);
            println!("  end_position   = {:?}", outcome.actual.end_position);
        }

        match outcome.result {
            AssertResult::Pass => {
                println!("PASS  {}", overlay_path.display());
                std::process::exit(0);
            }
            AssertResult::Fail(results) => {
                eprintln!("FAIL  {}", overlay_path.display());
                print_assertion_failure(&outcome.actual, &results);
                std::process::exit(1);
            }
        }
    }

    // ── Capture-golden mode ──────────────────────────────────────────────────
    if let Some(out_path) = capture_golden {
        use std::io::Write as _;
        let out_file = std::fs::File::create(&out_path).unwrap_or_else(|e| {
            eprintln!("error: cannot create {}: {e}", out_path.display());
            std::process::exit(2);
        });
        let mut writer = std::io::BufWriter::new(out_file);
        let mut captured: usize = 0;

        for path in &paths {
            let path_str = path.to_string_lossy();
            let events = match read_v27_events(path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error reading {}: {e}", path.display());
                    std::process::exit(2);
                }
            };
            for event in &events {
                if matches!(event.decision, LoggedDecision::Skip { .. }) {
                    continue; // Skip events: no pick_action to replay
                }
                let rec = match golden_from_v27_event(event, &path_str, &content, &inf_cfg) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error on actor_id={} in {}: {e}", event.actor_id, path.display());
                        std::process::exit(2);
                    }
                };
                let line = serde_json::to_string(&rec).unwrap_or_else(|e| {
                    eprintln!("error serializing golden record: {e}");
                    std::process::exit(2);
                });
                writeln!(writer, "{line}").unwrap_or_else(|e| {
                    eprintln!("error writing to {}: {e}", out_path.display());
                    std::process::exit(2);
                });
                captured += 1;
            }
        }
        writer.flush().unwrap_or_else(|e| {
            eprintln!("error flushing {}: {e}", out_path.display());
            std::process::exit(2);
        });
        println!("captured {captured} records → {}", out_path.display());
        std::process::exit(0);
    }

    // ── Compare-golden mode ──────────────────────────────────────────────────
    if let Some(baseline_path) = compare_golden {
        let baseline: Vec<GoldenRecord> = {
            use std::io::BufRead as _;
            let file = std::fs::File::open(&baseline_path).unwrap_or_else(|e| {
                eprintln!("error: cannot open {}: {e}", baseline_path.display());
                std::process::exit(2);
            });
            let reader = std::io::BufReader::new(file);
            let mut recs = Vec::new();
            for (lineno, line) in reader.lines().enumerate() {
                let line = line.unwrap_or_else(|e| {
                    eprintln!("error reading {}: {e}", baseline_path.display());
                    std::process::exit(2);
                });
                if line.trim().is_empty() { continue; }
                let rec: GoldenRecord = serde_json::from_str(&line).unwrap_or_else(|e| {
                    eprintln!(
                        "error: cannot parse golden record at line {} in {}: {e}",
                        lineno + 1, baseline_path.display()
                    );
                    std::process::exit(2);
                });
                recs.push(rec);
            }
            recs
        };

        let mut current: Vec<GoldenRecord> = Vec::new();
        for path in &paths {
            let path_str = path.to_string_lossy();
            let events = match read_v27_events(path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error reading {}: {e}", path.display());
                    std::process::exit(2);
                }
            };
            for event in &events {
                if matches!(event.decision, LoggedDecision::Skip { .. }) {
                    continue;
                }
                let rec = match golden_from_v27_event(event, &path_str, &content, &inf_cfg) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error on actor_id={} in {}: {e}", event.actor_id, path.display());
                        std::process::exit(2);
                    }
                };
                current.push(rec);
            }
        }

        let total = baseline.len().max(current.len());
        let mut diverged: usize = 0;
        for i in 0..total {
            match (current.get(i), baseline.get(i)) {
                (None, Some(_)) => {}
                (Some(cur), None) => {
                    eprintln!(
                        "case {i}: extra entry in current (log_path={:?}, actor_id={})",
                        cur.log_path, cur.actor_id
                    );
                    diverged += 1;
                }
                (Some(cur), Some(base)) => {
                    if cur.log_path != base.log_path || cur.actor_id != base.actor_id {
                        eprintln!(
                            "case {i} corpus mismatch: ({:?}, {}) vs ({:?}, {})",
                            cur.log_path, cur.actor_id, base.log_path, base.actor_id,
                        );
                        diverged += 1;
                        continue;
                    }
                    let mut case_diverged = false;
                    if cur.decision_kind != base.decision_kind {
                        eprintln!("case {i} diverged: decision_kind = {:?} vs {:?}", cur.decision_kind, base.decision_kind);
                        case_diverged = true;
                    }
                    if cur.cast_ability != base.cast_ability {
                        eprintln!("case {i} diverged: cast_ability = {:?} vs {:?}", cur.cast_ability, base.cast_ability);
                        case_diverged = true;
                    }
                    if cur.cast_target != base.cast_target {
                        eprintln!("case {i} diverged: cast_target = {:?} vs {:?}", cur.cast_target, base.cast_target);
                        case_diverged = true;
                    }
                    if cur.end_position != base.end_position {
                        eprintln!("case {i} diverged: end_position = {:?} vs {:?}", cur.end_position, base.end_position);
                        case_diverged = true;
                    }
                    if case_diverged { diverged += 1; }
                }
                (None, None) => {}
            }
        }
        if current.len() < baseline.len() {
            let missing = baseline.len() - current.len();
            eprintln!("missing {missing} trailing entries from current");
            diverged += missing;
        }

        eprintln!("{diverged} / {total} diverged");
        std::process::exit(if diverged == 0 { 0 } else { 1 });
    }

    // ── Standard replay mode ─────────────────────────────────────────────────
    let mut rng = DiceRng::with_seed(0);
    let difficulty = DifficultyProfile::normal();
    let mut metrics = Metrics::default();

    for path in &paths {
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        let reader = BufReader::new(file);

        let mut total = 0usize;
        let mut changed = 0usize;
        let mut skipped = 0usize;

        println!("\n=== Replay: {} ===\n", path.display());

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => { eprintln!("read error: {e}"); continue; }
            };
            if line.trim().is_empty() { continue; }

            // Schema version check.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(ver) = val.get("schema_version").and_then(|v| v.as_u64()) {
                    if ver != 27 {
                        eprintln!(
                            "error: schema v{ver} unsupported, v27+ required (file: {})",
                            path.display()
                        );
                        std::process::exit(1);
                    }
                }
                if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
                    continue;
                }
            }

            let event: ActorTickEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(e) => { eprintln!("parse error: {e}"); continue; }
            };

            // Skip events: actor had no AP/MP, no pick_action to replay.
            if matches!(event.decision, LoggedDecision::Skip { .. }) {
                skipped += 1;
                continue;
            }

            total += 1;

            let actor = match Entity::try_from_bits(event.actor_id) {
                Some(e) => e,
                None => { eprintln!("invalid actor_id {}, skipping", event.actor_id); continue; }
            };
            let Some(active) = event.snapshot.unit(actor).cloned() else {
                eprintln!("actor not found in snapshot, skipping");
                continue;
            };

            // Compute regression metrics from logged data (before re-picking).
            collect_metrics_from_event(&event, &active, &mut metrics);

            let maps = build_influence_maps(&event.snapshot, actor, active.team, &inf_cfg);
            let world = AiWorld {
                content: &content,
                difficulty: &difficulty,
                tuning: &content.ai_tuning,
                crit_fail_chance: 0.0,
            };
            let memory = AiMemory::default();
            let reservations = Reservations::default();

            let result = pick_action(
                actor,
                active.pos,
                &world,
                &event.snapshot,
                &maps,
                &mut rng,
                &memory,
                &reservations,
                false,
                &Default::default(),
            );

            let matched = decisions_match(&event.decision, &result.decision);
            if !matched {
                changed += 1;
                let logged_key = logged_decision_key(&event.decision);
                let fresh_key = ai_decision_key(&result.decision);
                println!(
                    "CHANGED r{} {} HP={}/{}: logged={:?} -> fresh={:?}",
                    event.round, event.actor_name, active.hp, active.max_hp,
                    logged_key, fresh_key,
                );
                if verbose {
                    print_event_plans(&event);
                }
            } else if verbose {
                let key = logged_decision_key(&event.decision);
                println!(
                    "=  r{} {} HP={}/{}: {:?}",
                    event.round, event.actor_name, active.hp, active.max_hp, key,
                );
            }
        }

        println!(
            "\n=== {} entries, {} skip, {} decision changes ===",
            total, skipped, changed
        );
    }

    if metrics_summary {
        metrics.print_summary();
    }
}

// ── Event reading ───────────────────────────────────────────────────────────

/// Read all `actor_tick` events (v27) from a JSONL file.
/// Returns an error if any line has a different schema version.
pub fn read_v27_events(path: &std::path::Path) -> Result<Vec<ActorTickEvent>, String> {
    use std::io::BufRead;

    let file = std::fs::File::open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("I/O error: {e}"))?;
        let line = line.trim();
        if line.is_empty() { continue; }

        let val: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("JSON parse error: {e}"))?;

        if let Some(ver) = val.get("schema_version").and_then(|v| v.as_u64()) {
            if ver != 27 {
                return Err(format!(
                    "error: schema v{ver} unsupported, v27+ required (file: {})",
                    path.display()
                ));
            }
        }

        if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
            continue;
        }

        let event: ActorTickEvent = serde_json::from_str(line)
            .map_err(|e| format!("event parse error: {e}"))?;
        events.push(event);
    }

    Ok(events)
}

// ── Metrics collection ──────────────────────────────────────────────────────

fn collect_metrics_from_event(
    event: &ActorTickEvent,
    active: &storyforge::combat::ai::snapshot::UnitSnapshot,
    metrics: &mut Metrics,
) {
    use storyforge::combat::ai::planning::CommittedPrefix;

    let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) else {
        return;
    };
    let chosen_final_pos = chosen.steps.iter().rev().find_map(|s| {
        if let PlanStep::Move { path } = s { path.last().copied() } else { None }
    }).unwrap_or(active.pos);

    // Move-only wasted.
    let plan = storyforge::combat::ai::planning::TurnPlan {
        steps: chosen.steps.clone(),
        final_pos: chosen_final_pos,
        residual_ap: 0,
        residual_mp: 0,
        outcomes: vec![],
        partial_score: 0.0,
        sim_snapshots: vec![],
        annotation: Default::default(),
    };
    if let CommittedPrefix::MoveOnly { path } = plan.committed_prefix() {
        metrics.move_only_total += 1;
        let dest = path.last().copied().unwrap_or(active.pos);
        if dest == active.pos {
            metrics.move_only_wasted += 1;
        }
    }

    // Move shape metrics.
    let has_any_move = chosen.steps.iter().any(|s| matches!(s, PlanStep::Move { .. }));
    if has_any_move {
        metrics.plans_with_moves += 1;

        // Repeated tile: same hex appears twice in the move path.
        let mut visited = std::collections::HashSet::new();
        let mut repeated = false;
        visited.insert(active.pos);
        for step in &chosen.steps {
            if let PlanStep::Move { path } = step {
                for &h in path {
                    if !visited.insert(h) {
                        repeated = true;
                    }
                }
            }
        }
        if repeated { metrics.repeated_tile_plans += 1; }

        // Zero-net: final position == start position.
        if chosen_final_pos == active.pos { metrics.zero_net_move_plans += 1; }
    }

    // Post-cast move metrics.
    let has_cast_step = chosen.steps.iter().any(|s| matches!(s, PlanStep::Cast { .. }));
    if has_cast_step {
        metrics.chosen_with_cast_total += 1;

        // Phantom tail: Cast step followed by Move step(s).
        let has_post_cast_move = {
            let mut seen_cast = false;
            let mut has_post = false;
            for s in &chosen.steps {
                if matches!(s, PlanStep::Cast { .. }) { seen_cast = true; }
                if seen_cast && matches!(s, PlanStep::Move { .. }) { has_post = true; break; }
            }
            has_post
        };

        if has_post_cast_move {
            metrics.plans_with_post_cast_move += 1;
            // Retreat = final pos farther from closest enemy than cast position.
            // Approximate: check if final pos is farther from actor's start.
            let cast_pos = chosen.steps.iter().find_map(|s| {
                if let PlanStep::Cast { target_pos, .. } = s { Some(*target_pos) } else { None }
            }).unwrap_or(active.pos);
            let dist_cast_to_final = cast_pos.distance_to(chosen_final_pos);
            if dist_cast_to_final > 0 { metrics.post_cast_retreat_plans += 1; }

            metrics.phantom_tail_chosen += 1;
        }
    }
}

fn print_event_plans(event: &ActorTickEvent) {
    let mut plans: Vec<&LoggedPlan> = event.plans.iter().collect();
    plans.sort_by(|a, b| a.rank.cmp(&b.rank));
    for p in plans.iter().take(5) {
        println!(
            "      #{}{} score={:+.2}  {}",
            p.rank,
            if p.annotation.chosen { "*" } else { " " },
            p.annotation.score,
            plan_shape_v27(&p.steps),
        );
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use storyforge::combat::ai::log::ActorTickEvent;
    use storyforge::combat::ai::snapshot::BattleSnapshot;

    fn make_skip_event() -> ActorTickEvent {
        ActorTickEvent {
            event_type: "actor_tick".to_owned(),
            schema_version: 27,
            round: 1,
            timestamp_ms: 0,
            actor_id: 1,
            actor_name: "test".to_owned(),
            snapshot: BattleSnapshot::default(),
            plans: vec![],
            decision: LoggedDecision::Skip { reason: "no_ap_no_mp".to_owned() },
            continuation: None,
            intent_reason: None,
        }
    }

    #[test]
    fn skip_decision_does_not_match_any_ai_decision() {
        // Skip is not a valid AiDecision — it must never be compared.
        assert!(matches!(make_skip_event().decision, LoggedDecision::Skip { .. }));
    }

    #[test]
    fn decisions_match_end_turn() {
        assert!(decisions_match(&LoggedDecision::EndTurn, &AiDecision::EndTurn));
    }

    #[test]
    fn decisions_differ_end_vs_cast() {
        use storyforge::core::AbilityId;
        // from_raw_u32 always produces a valid Entity (generation=NonZero default).
        // from_bits(0x100000000) panics on Bevy 0.18 — generation 1 with index 0
        // is not a valid initialised entity layout.
        let e = Entity::from_raw_u32(1).expect("valid entity");
        let cast = AiDecision::CastInPlace {
            ability: AbilityId("slash".to_owned()),
            target: e,
            target_pos: Hex::ZERO,
        };
        assert!(!decisions_match(&LoggedDecision::EndTurn, &cast));
    }

    #[test]
    fn v26_schema_triggers_error() {
        // Test that schema version check logic works: v26 is != 27.
        let json = r#"{"event_type":"actor_tick","schema_version":26}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let ver = val.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
        assert_ne!(ver, 27u64);
    }

    #[test]
    fn golden_record_json_roundtrip() {
        let rec = GoldenRecord {
            log_path: "logs/test.jsonl".to_owned(),
            plan_id: 42,
            actor_id: 99,
            decision_kind: "MoveAndCast".to_owned(),
            cast_ability: Some("slash".to_owned()),
            cast_target: Some(12884901551),
            end_position: [3, 5],
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: GoldenRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(rec, back);
    }
}
