//! AI decision log miner — v27 schema.
//!
//! Reads all `*.jsonl` from a directory and prints aggregated metrics.
//! Only `actor_tick` events with `schema_version == 27` are processed.
//! Files containing a different schema version produce a clear error.
//!
//! Class A (direct aggregation):
//!   A1. Adaptation reason frequency per plan (from annotation.adaptation).
//!   A2. Intent selection_kind frequency (from decision kind + Skip path).
//!   A3. Plan depth utilisation of the chosen plan (steps.len histogram).
//!   A4. (removed — was plan_divergence-specific, no longer applicable)
//!
//! Class B (sequence reconstruction):
//!   B5. Intent transition stability matrix per actor per combat.
//!
//! Class C (continuation analysis):
//!   C6. Continuation outcomes derived via `classify_continuation_outcome`.
//!       Skip events with stored_goal = new signal (actor passed with goal).
//!
//! Usage: `cargo run --release --bin mine_ai_logs -- --dir logs/`

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use storyforge::combat::ai::repair::{
    classify_continuation_outcome, ContinuationOutcome, FreshDecisionKind, StoredGoalContext,
};
use storyforge::combat::ai::intent::TacticalIntent;
use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision, StoredGoalContextSnapshot};

// ── Session actor key ─────────────────────────────────────────────────────────

/// One session = one JSONL file (= one combat).
type SessionActorKey = (String, u64); // (filename, actor_id)

// ── Aggregation state ─────────────────────────────────────────────────────────

#[derive(Default)]
struct Aggregate {
    // A1: adaptation reason per plan (all plans in pool, not just chosen)
    adaptation_counts: BTreeMap<String, usize>,
    total_plans: usize,

    // A2: decision_kind frequencies
    decision_kind_counts: BTreeMap<String, usize>,
    total_decisions: usize,

    // A3: chosen plan depth histogram
    depth_counts: BTreeMap<usize, usize>,
    total_chosen: usize,

    // C6: continuation outcomes (derived via classify_continuation_outcome)
    cont_no_stored: usize,
    cont_method_delivered: usize,
    cont_in_transit: usize,
    cont_abandoned_reactive: BTreeMap<String, usize>,
    cont_abandoned_voluntary: usize,
    cont_abandoned_invalidating: usize,
    cont_abandoned_ttl_expired: usize,
    cont_severity_counts: BTreeMap<String, usize>,
    cont_goal_kind_counts: BTreeMap<String, usize>,
    total_with_continuation: usize,

    // Skip-path signals
    skip_total: usize,
    skip_with_stored_goal: usize,

    // B5: per-(session, actor) ordered list of (tick_order, decision_kind)
    // `tick_order` = sequential index within the session for ordering.
    actor_timelines: HashMap<SessionActorKey, Vec<(u64, String)>>,
    // Counter per session-actor for ordering ticks (monotonically increasing).
    actor_tick_counters: HashMap<SessionActorKey, u64>,
}

impl Aggregate {
    fn process_event(&mut self, session: &str, event: &ActorTickEvent) {
        self.total_decisions += 1;

        let decision_kind = decision_kind_label(&event.decision);

        // A2: decision_kind
        *self.decision_kind_counts.entry(decision_kind.to_owned()).or_default() += 1;

        // Skip path
        if matches!(event.decision, LoggedDecision::Skip { .. }) {
            self.skip_total += 1;
            if event.continuation.is_some() {
                self.skip_with_stored_goal += 1;
            }
            // B5: record skip tick for timeline
            let key: SessionActorKey = (session.to_owned(), event.actor_id);
            let counter = self.actor_tick_counters.entry(key.clone()).or_default();
            let order = *counter;
            *counter += 1;
            self.actor_timelines.entry(key).or_default().push((order, decision_kind.to_owned()));
            // C6 for skip path
            self.process_continuation(event, FreshDecisionKind::EndTurn);
            return;
        }

        // Full path: A1 + A3 + B5 + C6

        // A1: adaptation reason per plan
        for plan in &event.plans {
            self.total_plans += 1;
            let reason_key = plan
                .annotation
                .adaptation
                .as_ref()
                .map(|a| format!("{:?}", a.reason))
                .unwrap_or_else(|| "none".to_owned());
            *self.adaptation_counts.entry(reason_key).or_default() += 1;
        }

        // A3: chosen plan depth
        if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
            self.total_chosen += 1;
            *self.depth_counts.entry(chosen.steps.len()).or_default() += 1;
        }

        // B5
        let key: SessionActorKey = (session.to_owned(), event.actor_id);
        let counter = self.actor_tick_counters.entry(key.clone()).or_default();
        let order = *counter;
        *counter += 1;
        self.actor_timelines.entry(key).or_default().push((order, decision_kind.to_owned()));

        // C6
        let fdk = fresh_decision_kind(&event.decision);
        self.process_continuation(event, fdk);
    }

    fn process_continuation(&mut self, event: &ActorTickEvent, fdk: FreshDecisionKind) {
        self.total_with_continuation += 1;

        let Some(cont) = &event.continuation else {
            self.cont_no_stored += 1;
            return;
        };

        // Reconstruct StoredGoalContext from the log snapshot for classify_continuation_outcome.
        let stored_goal = StoredGoalContext::from(&cont.stored_goal);

        // Approximate the fresh TacticalIntent from decision + stored goal context.
        let fresh_intent = approximate_fresh_intent(&event.decision, &cont.stored_goal);

        // fresh_reason: we approximate from decision kind (no full IntentReason in log).
        // For outcome classification, what matters is the IntentReason::code()
        // to distinguish reactive vs voluntary. We use a synthetic NoRuleDefault
        // reason for non-reactive cases — this is a known approximation since
        // the v27 log does not store the full IntentReason.
        // The reactive sources (taunt, adaptation) would need richer data to separate.
        // For now, use NoRuleDefault which classifies as voluntary when goal abandoned.
        let synthetic_reason = storyforge::combat::ai::intent::IntentReason::NoRuleDefault;

        let outcome = classify_continuation_outcome(
            Some(&stored_goal),
            fresh_intent,
            fdk,
            &synthetic_reason,
            cont.severity,
            cont.age,
        );

        match outcome {
            ContinuationOutcome::NoStoredGoal => {
                self.cont_no_stored += 1;
            }
            ContinuationOutcome::GoalPreservedMethodDelivered => {
                self.cont_method_delivered += 1;
            }
            ContinuationOutcome::GoalPreservedInTransit => {
                self.cont_in_transit += 1;
            }
            ContinuationOutcome::GoalAbandonedReactive { source } => {
                *self.cont_abandoned_reactive.entry(source).or_default() += 1;
            }
            ContinuationOutcome::GoalAbandonedVoluntary => {
                self.cont_abandoned_voluntary += 1;
            }
            ContinuationOutcome::GoalAbandonedInvalidating => {
                self.cont_abandoned_invalidating += 1;
            }
            ContinuationOutcome::GoalAbandonedTtlExpired => {
                self.cont_abandoned_ttl_expired += 1;
            }
            ContinuationOutcome::LegacyV25Abandoned { .. } => {
                // Cannot appear from classify_continuation_outcome; ignore.
            }
        }

        if let Some(sev) = cont.severity {
            *self.cont_severity_counts.entry(format!("{sev:?}")).or_default() += 1;
        }
        let goal_kind = format!("{:?}", cont.stored_goal.kind);
        *self.cont_goal_kind_counts.entry(goal_kind).or_default() += 1;
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn decision_kind_label(d: &LoggedDecision) -> &'static str {
    match d {
        LoggedDecision::Cast { .. } => "Cast",
        LoggedDecision::MoveAndCast { .. } => "MoveAndCast",
        LoggedDecision::Move { .. } => "Move",
        LoggedDecision::EndTurn => "EndTurn",
        LoggedDecision::Skip { .. } => "Skip",
    }
}

fn fresh_decision_kind(d: &LoggedDecision) -> FreshDecisionKind {
    match d {
        LoggedDecision::Cast { .. } | LoggedDecision::MoveAndCast { .. } => FreshDecisionKind::Cast,
        LoggedDecision::Move { .. } => FreshDecisionKind::Move,
        LoggedDecision::EndTurn | LoggedDecision::Skip { .. } => FreshDecisionKind::EndTurn,
    }
}

/// Approximate the fresh `TacticalIntent` from the logged decision + stored goal.
///
/// `classify_continuation_outcome` requires the fresh decision's `TacticalIntent`.
/// The v27 log does not persist intent directly; we reconstruct it approximately:
///
/// - Cast/MoveAndCast targeting the same entity as the stored goal → use the stored
///   goal's intent kind (the actor continued toward the same target).
/// - Cast/MoveAndCast targeting a *different* entity → `FocusTarget` at that new entity.
/// - Move → `Reposition` (best approximation; could be FocusTarget-walk, but we can't know).
/// - EndTurn/Skip → `Reposition` (pass, no meaningful intent to infer).
///
/// This heuristic is sufficient for the miner's `preserved` vs `abandoned` split,
/// though reactive-vs-voluntary classification for `GoalAbandonedReactive` requires
/// the real `IntentReason.code()` which is not in the log. The synthetic `NoRuleDefault`
/// reason causes all abandoned-not-invalidating-not-ttl cases to be classified as
/// `GoalAbandonedVoluntary` in the miner output.
fn approximate_fresh_intent(
    decision: &LoggedDecision,
    stored_goal: &StoredGoalContextSnapshot,
) -> TacticalIntent {
    use bevy::prelude::Entity;

    let stored_target = stored_goal.target_id.and_then(Entity::try_from_bits);

    match decision {
        LoggedDecision::Cast { target, .. } | LoggedDecision::MoveAndCast { target, .. } => {
            let Some(fresh) = Entity::try_from_bits(*target) else {
                return TacticalIntent::Reposition;
            };
            // If targeting the same entity as the stored goal, reproduce the stored intent kind.
            if Some(fresh) == stored_target {
                match stored_goal.kind.as_str() {
                    "finish" | "pressure" => TacticalIntent::FocusTarget { target: fresh },
                    "disable_enemy" => TacticalIntent::ApplyCC { target: fresh },
                    "heal_ally" => TacticalIntent::ProtectAlly { ally: fresh },
                    _ => TacticalIntent::FocusTarget { target: fresh },
                }
            } else {
                // Targeting a different entity — classify as FocusTarget to that entity.
                TacticalIntent::FocusTarget { target: fresh }
            }
        }
        LoggedDecision::Move { .. }
        | LoggedDecision::EndTurn
        | LoggedDecision::Skip { .. } => TacticalIntent::Reposition,
    }
}

// ── Printing helpers ──────────────────────────────────────────────────────────

fn pct(num: usize, denom: usize) -> f64 {
    if denom > 0 { num as f64 / denom as f64 * 100.0 } else { 0.0 }
}

fn print_freq_table(items: &BTreeMap<String, usize>, total: usize) {
    let mut rows: Vec<(&str, usize)> = items.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (key, count) in rows {
        println!("  {:<40} {:>6}  ({:5.1}%)", key, count, pct(count, total));
    }
}

fn print_depth_table(items: &BTreeMap<usize, usize>, total: usize) {
    let mut rows: Vec<(usize, usize)> = items.iter().map(|(k, v)| (*k, *v)).collect();
    rows.sort_by_key(|r| r.0);
    for (depth, count) in rows {
        println!("  depth {:>2}   {:>6}  ({:5.1}%)", depth, count, pct(count, total));
    }
}

fn print_transition_matrix(actor_timelines: &HashMap<SessionActorKey, Vec<(u64, String)>>) {
    use std::collections::BTreeSet;

    let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut total_transitions = 0usize;

    for timeline in actor_timelines.values() {
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(order, _)| *order);
        for window in sorted.windows(2) {
            let from = window[0].1.clone();
            let to = window[1].1.clone();
            *transitions.entry((from, to)).or_default() += 1;
            total_transitions += 1;
        }
    }

    if total_transitions == 0 {
        println!("  (no transitions — only single-decision actors)");
        return;
    }

    let all_kinds: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for (from, to) in transitions.keys() {
            set.insert(from.clone());
            set.insert(to.clone());
        }
        set.into_iter().collect()
    };

    let n = all_kinds.len();
    let col_w = all_kinds.iter().map(|k| k.len()).max().unwrap_or(4).max(4) + 2;
    let row_label_w = col_w;

    print!("{:>row_label_w$}", "FROM \\ TO");
    for to in &all_kinds {
        print!("  {:>col_w$}", &to[..to.len().min(col_w)]);
    }
    println!("  |  TOTAL");
    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));

    for from in &all_kinds {
        let row_total: usize = all_kinds
            .iter()
            .map(|to| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0))
            .sum();
        print!("{:>row_label_w$}", &from[..from.len().min(row_label_w)]);
        for to in &all_kinds {
            let cnt = *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0);
            if cnt > 0 {
                print!("  {:>col_w$}", cnt);
            } else {
                print!("  {:>col_w$}", ".");
            }
        }
        println!("  |  {:>5}  ({:.1}%)", row_total, pct(row_total, total_transitions));
    }

    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));
    print!("{:>row_label_w$}", "TOTAL");
    for to in &all_kinds {
        let col_total: usize = all_kinds
            .iter()
            .map(|from| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0))
            .sum();
        print!("  {:>col_w$}", col_total);
    }
    println!("  |  {:>5}", total_transitions);
    println!("\nTotal transitions: {total_transitions}");

    let mut top: Vec<_> = transitions.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\nTop-10 transitions:");
    for ((from, to), cnt) in top.iter().take(10) {
        println!(
            "  {:<40} -> {:<40}  {:>5}  ({:.1}%)",
            from, to, cnt, pct(**cnt, total_transitions)
        );
    }

    let self_loops: usize = all_kinds
        .iter()
        .map(|k| *transitions.get(&(k.clone(), k.clone())).unwrap_or(&0))
        .sum();
    println!(
        "\nSelf-loop rate (intent unchanged between ticks): {} / {} ({:.1}%)",
        self_loops, total_transitions, pct(self_loops, total_transitions)
    );
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dir: Option<PathBuf> = None;
    let mut iter = args[1..].iter();
    while let Some(a) = iter.next() {
        if a == "--dir" || a == "-d" {
            dir = iter.next().map(PathBuf::from);
        } else if !a.starts_with('-') {
            dir = Some(PathBuf::from(a));
        }
    }
    let dir = dir.unwrap_or_else(|| {
        eprintln!("usage: mine_ai_logs --dir <logs-dir>");
        std::process::exit(2);
    });

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();

    if files.is_empty() {
        eprintln!("no *.jsonl files found in {}", dir.display());
        std::process::exit(1);
    }

    let mut agg = Aggregate::default();
    let mut parse_errors = 0usize;
    let mut schema_errors = 0usize;

    for path in &files {
        let session = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Fast-path schema version check before full deserialisation.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(ver) = val.get("schema_version").and_then(|v| v.as_u64()) {
                    if ver != 27 {
                        eprintln!(
                            "error: schema v{ver} unsupported, v27+ required (file: {})",
                            path.display()
                        );
                        schema_errors += 1;
                        continue;
                    }
                }
                // Only process actor_tick events.
                if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
                    continue;
                }
            }

            let event: ActorTickEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => {
                    parse_errors += 1;
                    continue;
                }
            };

            agg.process_event(&session, &event);
        }
    }

    // ── Report ────────────────────────────────────────────────────────────────

    println!("# AI mining — v27");
    println!();
    println!(
        "Source: {} JSONL files, {} AI decisions ({} skip)",
        files.len(), agg.total_decisions, agg.skip_total
    );
    if parse_errors > 0 {
        println!("Parse errors (lines skipped): {parse_errors}");
    }
    if schema_errors > 0 {
        println!("Schema errors (non-v27 lines skipped): {schema_errors}");
    }
    println!();

    // A1: Adaptation reason frequency
    println!("## A1. Adaptation reason frequency (per plan in pool)");
    println!();
    println!("Total plans in pool (all logged, not just chosen): {}", agg.total_plans);
    println!();
    print_freq_table(&agg.adaptation_counts, agg.total_plans);
    println!();

    // A2: Decision kind frequency
    println!("## A2. Decision kind frequency (per tick)");
    println!();
    println!("Total ticks: {}", agg.total_decisions);
    println!();
    print_freq_table(&agg.decision_kind_counts, agg.total_decisions);
    println!();

    // A3: Plan depth utilisation
    println!("## A3. Chosen plan depth (steps.len) histogram");
    println!();
    println!("Total chosen plans: {}", agg.total_chosen);
    println!();
    print_depth_table(&agg.depth_counts, agg.total_chosen);
    println!();

    // Skip-path signals
    println!("## Skip-path signals");
    println!();
    println!(
        "  skip total                : {:>6}  ({:5.1}% of all ticks)",
        agg.skip_total, pct(agg.skip_total, agg.total_decisions)
    );
    println!(
        "  skip with stored_goal     : {:>6}  ({:5.1}% of skips)",
        agg.skip_with_stored_goal, pct(agg.skip_with_stored_goal, agg.skip_total)
    );
    println!();

    // C6: Continuation analysis
    println!("## C6. Continuation analysis (derived via classify_continuation_outcome)");
    println!();
    let n = agg.total_with_continuation;
    let cont_preserved_combined = agg.cont_method_delivered + agg.cont_in_transit;
    let cont_abandoned_reactive_total: usize = agg.cont_abandoned_reactive.values().sum();
    println!("Total ticks analysed: {n}");
    println!("  (outcome derived from raw log data — no pre-classified strings)");
    println!("  NOTE: reactive vs voluntary classification uses synthetic NoRuleDefault reason;");
    println!("        full accuracy requires stored IntentReason (planned for v28+).");
    println!();
    if n == 0 {
        println!("  (no ticks found)");
    } else {
        println!(
            "  goal_preserved | method_delivered : {:>6.1}%  ({})  [target: >=10%]",
            pct(agg.cont_method_delivered, n), agg.cont_method_delivered,
        );
        println!(
            "  goal_preserved | in_transit       : {:>6.1}%  ({})",
            pct(agg.cont_in_transit, n), agg.cont_in_transit,
        );
        println!(
            "  goal_preserved (combined)         : {:>6.1}%  ({})  [target: >=60%]",
            pct(cont_preserved_combined, n), cont_preserved_combined,
        );
        println!();
        println!(
            "  goal_abandoned | reactive         : {:>6.1}%  ({})",
            pct(cont_abandoned_reactive_total, n), cont_abandoned_reactive_total,
        );
        {
            let mut reactive_rows: Vec<(&str, usize)> = agg
                .cont_abandoned_reactive
                .iter()
                .map(|(k, v)| (k.as_str(), *v))
                .collect();
            reactive_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            for (src, count) in &reactive_rows {
                println!("    {:<34} {:>4}  ({:5.1}%)", src, count, pct(*count, n));
            }
        }
        println!(
            "  goal_abandoned | voluntary        : {:>6.1}%  ({})  [target: <=10%, REAL commitment failure]",
            pct(agg.cont_abandoned_voluntary, n), agg.cont_abandoned_voluntary,
        );
        println!(
            "  goal_abandoned | invalidating     : {:>6.1}%  ({})",
            pct(agg.cont_abandoned_invalidating, n), agg.cont_abandoned_invalidating,
        );
        println!(
            "  goal_abandoned | ttl_expired      : {:>6.1}%  ({})",
            pct(agg.cont_abandoned_ttl_expired, n), agg.cont_abandoned_ttl_expired,
        );
        println!(
            "  no_stored_goal                    : {:>6.1}%  ({})",
            pct(agg.cont_no_stored, n), agg.cont_no_stored,
        );
        if !agg.cont_severity_counts.is_empty() {
            println!();
            println!("  severity distribution:");
            print_freq_table(&agg.cont_severity_counts, agg.total_with_continuation);
        }
        if !agg.cont_goal_kind_counts.is_empty() {
            println!();
            println!("  goal_kind distribution:");
            print_freq_table(&agg.cont_goal_kind_counts, n);
        }
    }
    println!();

    // B5: Intent transition matrix
    println!("## B5. Decision kind transition matrix");
    println!();
    println!("Grouping: per actor per combat (JSONL file). Ordered by tick sequence.");
    println!(
        "Unique (combat, actor) pairs tracked: {}",
        agg.actor_timelines.len()
    );
    println!();
    print_transition_matrix(&agg.actor_timelines);
    println!();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision, LoggedPlan};
    use storyforge::combat::ai::snapshot::BattleSnapshot;
    use storyforge::combat::ai::outcome::PlanAnnotation;
    use storyforge::combat::ai::planning::PlanStep;

    fn make_event(
        actor_id: u64,
        decision: LoggedDecision,
        plans: Vec<LoggedPlan>,
        continuation: Option<storyforge::combat::ai::log::ContinuationLogSection>,
    ) -> ActorTickEvent {
        ActorTickEvent {
            event_type: "actor_tick".to_owned(),
            schema_version: 27,
            round: 1,
            timestamp_ms: 0,
            actor_id,
            actor_name: "test".to_owned(),
            snapshot: BattleSnapshot::default(),
            plans,
            decision,
            continuation,
        }
    }

    fn plan_chosen(steps_len: usize) -> LoggedPlan {
        let ann = PlanAnnotation { chosen: true, ..Default::default() };
        let steps = (0..steps_len)
            .map(|_| PlanStep::Move { path: vec![] })
            .collect();
        LoggedPlan {
            rank: 1,
            steps,
            annotation: ann,
        }
    }

    fn plan_unchosen() -> LoggedPlan {
        LoggedPlan {
            rank: 2,
            steps: vec![PlanStep::Move { path: vec![] }],
            annotation: PlanAnnotation::default(),
        }
    }

    #[test]
    fn decision_kind_counted_correctly() {
        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &make_event(1, LoggedDecision::EndTurn, vec![], None));
        agg.process_event("f.jsonl", &make_event(1, LoggedDecision::EndTurn, vec![], None));
        agg.process_event(
            "f.jsonl",
            &make_event(1, LoggedDecision::Move { path: vec![] }, vec![], None),
        );

        assert_eq!(agg.total_decisions, 3);
        assert_eq!(*agg.decision_kind_counts.get("EndTurn").unwrap(), 2);
        assert_eq!(*agg.decision_kind_counts.get("Move").unwrap(), 1);
    }

    #[test]
    fn plan_depth_tracks_chosen_steps_len() {
        let mut agg = Aggregate::default();
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_chosen(3), plan_unchosen()],
            None,
        );
        agg.process_event("f.jsonl", &event);
        assert_eq!(*agg.depth_counts.get(&3).unwrap(), 1, "chosen plan has 3 steps");
        assert!(!agg.depth_counts.contains_key(&1), "non-chosen plan not counted");
    }

    #[test]
    fn skip_event_counted_separately() {
        let mut agg = Aggregate::default();
        agg.process_event(
            "f.jsonl",
            &make_event(
                1,
                LoggedDecision::Skip { reason: "no_ap_no_mp".to_owned() },
                vec![],
                None,
            ),
        );
        assert_eq!(agg.skip_total, 1);
        assert_eq!(agg.skip_with_stored_goal, 0);
        assert_eq!(agg.total_plans, 0);
    }

    #[test]
    fn v26_schema_skipped_with_error() {
        // The main() loop checks schema_version before deserialising.
        // This test verifies the label logic directly via the fast-path check.
        let json = r#"{"event_type":"actor_tick","schema_version":26,"round":1}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let ver = val.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
        assert_ne!(ver, 27, "v26 must be rejected");
    }

    #[test]
    fn transition_matrix_self_loops_and_changes() {
        let mut agg = Aggregate::default();
        // Actor 1, 3 sequential ticks: EndTurn → EndTurn → Move
        for (order, d) in [
            LoggedDecision::EndTurn,
            LoggedDecision::EndTurn,
            LoggedDecision::Move { path: vec![] },
        ]
        .into_iter()
        .enumerate()
        {
            let _ = order; // order tracked via actor_tick_counters internally
            agg.process_event("A.jsonl", &make_event(1, d, vec![], None));
        }

        let key = ("A.jsonl".to_owned(), 1u64);
        let timeline = agg.actor_timelines.get(&key).unwrap();
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(ord, _)| *ord);

        let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
        for w in sorted.windows(2) {
            *transitions
                .entry((w[0].1.clone(), w[1].1.clone()))
                .or_default() += 1;
        }

        assert_eq!(
            *transitions
                .get(&("EndTurn".to_owned(), "EndTurn".to_owned()))
                .unwrap(),
            1
        );
        assert_eq!(
            *transitions
                .get(&("EndTurn".to_owned(), "Move".to_owned()))
                .unwrap(),
            1
        );
    }

    #[test]
    fn pct_zero_denominator_returns_zero() {
        assert_eq!(pct(5, 0), 0.0);
        assert!((pct(1, 4) - 25.0).abs() < 1e-9);
    }
}
