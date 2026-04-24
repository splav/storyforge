//! AI decision log miner (step 0.3 A+B).
//!
//! Reads all `*.jsonl` from a directory and prints aggregated metrics:
//!
//! Class A (direct aggregation):
//!   A1. Adaptation reason frequency per plan.
//!   A2. Panic-override frequency (selection_kind=="panic_override") vs all decisions.
//!   A3. Plan depth utilisation of the chosen plan (steps.len histogram).
//!   A4. Continuation invalidation reasons (replan_reason from plan_divergence entries).
//!
//! Class B (sequence reconstruction):
//!   B5. Intent transition stability matrix and top transitions per actor per combat.
//!
//! Usage: `cargo run --release --bin mine_ai_logs -- --dir logs/`

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::Deserialize;

// ── Minimal log schema mirrors ────────────────────────────────────────────────
//
// We only pull the fields we need for mining — the rest are ignored by serde.
// This keeps the miner independent of all Bevy / game types.

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    event_type: Option<String>,

    // pick_action fields
    #[serde(default)]
    plan_id: u64,
    #[serde(default)]
    actor_id: u64,
    #[serde(default)]
    intent: Option<IntentBlock>,
    #[serde(default)]
    plans: Vec<PlanLog>,

    // plan_divergence fields
    #[serde(default)]
    replan_reason: Option<String>,
    #[serde(default)]
    used_continuation: bool,
}

#[derive(Deserialize)]
struct IntentBlock {
    selection_kind: String,
}

#[derive(Deserialize)]
struct PlanLog {
    #[serde(default)]
    chosen: bool,
    #[serde(default)]
    steps: Vec<serde_json::Value>, // we only need len, so Value is fine
    #[serde(default)]
    adaptation_reason: Option<AdaptationReason>,
}

#[derive(Deserialize)]
struct AdaptationReason {
    kind: String,
}

// ── Aggregation state ─────────────────────────────────────────────────────────

/// One session = one JSONL file (= one combat).
/// Tracks per-actor (plan_id, selection_kind) pairs in order for transition matrix.
type SessionActorKey = (String, u64); // (filename, actor_id)

#[derive(Default)]
struct Aggregate {
    // A1: adaptation_reason per plan (all plans in pool, not just chosen)
    adaptation_counts: BTreeMap<String, usize>,
    total_plans: usize,

    // A2: selection_kind frequencies for every pick_action entry
    selection_kind_counts: BTreeMap<String, usize>,
    total_decisions: usize,

    // A3: chosen plan depth histogram
    depth_counts: BTreeMap<usize, usize>,
    total_chosen: usize,

    // A4: replan_reason from plan_divergence entries
    replan_reason_counts: BTreeMap<String, usize>,
    total_divergences: usize,

    // B5: per-(session, actor) ordered list of (plan_id, selection_kind)
    // Stored as Vec keyed by (session, actor) — built in load pass, consumed in B5.
    actor_timelines: HashMap<SessionActorKey, Vec<(u64, String)>>,
}

impl Aggregate {
    fn process_pick_action(&mut self, session: &str, entry: &Entry) {
        self.total_decisions += 1;

        // A2: selection_kind
        let kind = entry
            .intent
            .as_ref()
            .map(|i| i.selection_kind.as_str())
            .unwrap_or("missing");
        *self.selection_kind_counts.entry(kind.to_owned()).or_default() += 1;

        // A1: adaptation_reason per plan in pool
        for plan in &entry.plans {
            self.total_plans += 1;
            let reason_key = match &plan.adaptation_reason {
                None => "none".to_owned(),
                Some(r) => r.kind.clone(),
            };
            *self.adaptation_counts.entry(reason_key).or_default() += 1;
        }

        // A3: chosen plan depth
        if let Some(chosen) = entry.plans.iter().find(|p| p.chosen) {
            self.total_chosen += 1;
            *self.depth_counts.entry(chosen.steps.len()).or_default() += 1;
        }

        // B5: record (plan_id, selection_kind) for this actor in this session
        let timeline = self
            .actor_timelines
            .entry((session.to_owned(), entry.actor_id))
            .or_default();
        timeline.push((entry.plan_id, kind.to_owned()));
    }

    fn process_plan_divergence(&mut self, entry: &Entry) {
        self.total_divergences += 1;
        let reason = match &entry.replan_reason {
            Some(r) => r.clone(),
            None => {
                if entry.used_continuation {
                    "used_continuation".to_owned()
                } else {
                    "none".to_owned()
                }
            }
        };
        *self.replan_reason_counts.entry(reason).or_default() += 1;
    }
}

// ── Printing helpers ──────────────────────────────────────────────────────────

fn pct(num: usize, denom: usize) -> f64 {
    if denom > 0 { num as f64 / denom as f64 * 100.0 } else { 0.0 }
}

/// Print a frequency table sorted by count descending, with percentage.
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

/// Build and print the intent transition matrix (B5).
fn print_transition_matrix(actor_timelines: &HashMap<SessionActorKey, Vec<(u64, String)>>) {
    use std::collections::BTreeSet;

    // Build ordered timelines and collect transitions.
    let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut total_transitions = 0usize;

    for timeline in actor_timelines.values() {
        // Sort by plan_id (ascending) within the same session-actor sequence.
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(pid, _)| *pid);

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

    // Collect all intent kinds that appear in the matrix.
    let all_kinds: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for (from, to) in transitions.keys() {
            set.insert(from.clone());
            set.insert(to.clone());
        }
        set.into_iter().collect()
    };

    let n = all_kinds.len();
    // Column header width: longest kind name + 2.
    let col_w = all_kinds.iter().map(|k| k.len()).max().unwrap_or(4).max(4) + 2;
    let row_label_w = col_w;

    // Header row.
    print!("{:>row_label_w$}", "FROM \\ TO");
    for to in &all_kinds {
        print!("  {:>col_w$}", &to[..to.len().min(col_w)]);
    }
    println!("  |  TOTAL");

    // Separator.
    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));

    // Data rows.
    for from in &all_kinds {
        let row_total: usize = all_kinds.iter().map(|to| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0)).sum();
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

    // Footer: column totals.
    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));
    print!("{:>row_label_w$}", "TOTAL");
    for to in &all_kinds {
        let col_total: usize = all_kinds.iter().map(|from| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0)).sum();
        print!("  {:>col_w$}", col_total);
    }
    println!("  |  {:>5}", total_transitions);

    println!("\nTotal transitions: {total_transitions}");

    // Top-5 most frequent (from → to) pairs.
    let mut top: Vec<_> = transitions.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\nTop-10 transitions:");
    for ((from, to), cnt) in top.iter().take(10) {
        println!("  {:<40} -> {:<40}  {:>5}  ({:.1}%)", from, to, cnt, pct(**cnt, total_transitions));
    }

    // Self-loop rate (intent stayed the same tick-over-tick).
    let self_loops: usize = all_kinds.iter().map(|k| *transitions.get(&(k.clone(), k.clone())).unwrap_or(&0)).sum();
    println!("\nSelf-loop rate (intent unchanged between ticks): {} / {} ({:.1}%)", self_loops, total_transitions, pct(self_loops, total_transitions));
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

    // Collect and sort JSONL files for deterministic output.
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
            let entry: Entry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => {
                    parse_errors += 1;
                    continue;
                }
            };
            match entry.event_type.as_deref() {
                Some("plan_divergence") => agg.process_plan_divergence(&entry),
                None | Some("pick_action") | Some("") => {
                    // Regular pick_action entries: event_type is absent (older logs)
                    // or explicitly "pick_action". Skip if intent is missing.
                    if entry.intent.is_some() {
                        agg.process_pick_action(&session, &entry);
                    }
                }
                Some(_) => {} // unknown event type, skip gracefully
            }
        }
    }

    // ── Report ────────────────────────────────────────────────────────────────

    println!("# AI mining — step 0.3 A+B");
    println!();
    println!("Source: {} JSONL files, {} AI decisions, {} plan_divergence entries",
        files.len(), agg.total_decisions, agg.total_divergences);
    if parse_errors > 0 {
        println!("Parse errors (lines skipped): {parse_errors}");
    }
    println!();

    // A1: Adaptation reason frequency
    println!("## A1. Adaptation reason frequency (per plan in pool)");
    println!();
    println!("Total plans in pool (all logged, not just chosen): {}", agg.total_plans);
    println!();
    print_freq_table(&agg.adaptation_counts, agg.total_plans);
    println!();

    // A2: Selection kind (intent selection) frequency
    println!("## A2. Intent selection_kind frequency (per decision)");
    println!();
    println!("Total decisions: {}", agg.total_decisions);
    println!();
    print_freq_table(&agg.selection_kind_counts, agg.total_decisions);
    println!();

    // A3: Plan depth utilisation
    println!("## A3. Chosen plan depth (steps.len) histogram");
    println!();
    println!("Total chosen plans: {}", agg.total_chosen);
    println!();
    print_depth_table(&agg.depth_counts, agg.total_chosen);
    println!();

    // A4: Continuation invalidation reasons
    println!("## A4. Continuation invalidation reasons (plan_divergence entries)");
    println!();
    println!("Total plan_divergence entries: {}", agg.total_divergences);
    println!();
    if agg.replan_reason_counts.is_empty() {
        println!("  (no plan_divergence entries found)");
    } else {
        print_freq_table(&agg.replan_reason_counts, agg.total_divergences);
    }
    println!();

    // B5: Intent transition matrix
    println!("## B5. Intent transition stability matrix");
    println!();
    println!("Grouping: per actor per combat (JSONL file). Ordered by plan_id.");
    println!("Unique (combat, actor) pairs tracked: {}", agg.actor_timelines.len());
    println!();
    print_transition_matrix(&agg.actor_timelines);
    println!();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agg() -> Aggregate {
        Aggregate::default()
    }

    #[test]
    fn adaptation_reason_counts_none_and_kinds() {
        let mut agg = make_agg();
        // entry with 2 plans: one with reason, one without
        let entry = Entry {
            event_type: None,
            plan_id: 0,
            actor_id: 1,
            intent: Some(IntentBlock { selection_kind: "best_priority".to_owned() }),
            plans: vec![
                PlanLog { chosen: true, steps: vec![], adaptation_reason: None },
                PlanLog {
                    chosen: false,
                    steps: vec![],
                    adaptation_reason: Some(AdaptationReason { kind: "expected_self_lethal".to_owned() }),
                },
            ],
            replan_reason: None,
            used_continuation: false,
        };
        agg.process_pick_action("f.jsonl", &entry);
        assert_eq!(agg.total_plans, 2);
        assert_eq!(*agg.adaptation_counts.get("none").unwrap(), 1);
        assert_eq!(*agg.adaptation_counts.get("expected_self_lethal").unwrap(), 1);
    }

    #[test]
    fn plan_depth_tracks_chosen_steps_len() {
        let mut agg = make_agg();
        let entry = Entry {
            event_type: None,
            plan_id: 0,
            actor_id: 1,
            intent: Some(IntentBlock { selection_kind: "killable".to_owned() }),
            plans: vec![
                PlanLog {
                    chosen: true,
                    steps: vec![serde_json::Value::Null, serde_json::Value::Null],
                    adaptation_reason: None,
                },
                PlanLog { chosen: false, steps: vec![serde_json::Value::Null], adaptation_reason: None },
            ],
            replan_reason: None,
            used_continuation: false,
        };
        agg.process_pick_action("f.jsonl", &entry);
        assert_eq!(*agg.depth_counts.get(&2).unwrap(), 1, "chosen plan has 2 steps");
        assert!(!agg.depth_counts.contains_key(&1), "non-chosen plan not counted");
    }

    #[test]
    fn replan_reason_counted_for_divergence() {
        let mut agg = make_agg();
        let entry = Entry {
            event_type: Some("plan_divergence".to_owned()),
            plan_id: 0,
            actor_id: 1,
            intent: None,
            plans: vec![],
            replan_reason: Some("actor_hp_drop".to_owned()),
            used_continuation: false,
        };
        agg.process_plan_divergence(&entry);
        assert_eq!(agg.total_divergences, 1);
        assert_eq!(*agg.replan_reason_counts.get("actor_hp_drop").unwrap(), 1);
    }

    #[test]
    fn transition_matrix_self_loops_and_changes() {
        let mut agg = make_agg();
        // Actor 1 in session A: best_priority → best_priority → killable
        let entries = vec![
            ("A.jsonl", 1u64, 0u64, "best_priority"),
            ("A.jsonl", 1u64, 1u64, "best_priority"),
            ("A.jsonl", 1u64, 2u64, "killable"),
        ];
        for (session, actor, plan_id, kind) in entries {
            let entry = Entry {
                event_type: None,
                plan_id,
                actor_id: actor,
                intent: Some(IntentBlock { selection_kind: kind.to_owned() }),
                plans: vec![PlanLog { chosen: true, steps: vec![], adaptation_reason: None }],
                replan_reason: None,
                used_continuation: false,
            };
            agg.process_pick_action(session, &entry);
        }

        // Build transitions manually to verify.
        let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
        let timeline = agg.actor_timelines.get(&("A.jsonl".to_owned(), 1u64)).unwrap();
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(pid, _)| *pid);
        for w in sorted.windows(2) {
            *transitions.entry((w[0].1.clone(), w[1].1.clone())).or_default() += 1;
        }

        assert_eq!(*transitions.get(&("best_priority".to_owned(), "best_priority".to_owned())).unwrap(), 1);
        assert_eq!(*transitions.get(&("best_priority".to_owned(), "killable".to_owned())).unwrap(), 1);
        assert!(!transitions.contains_key(&("killable".to_owned(), "best_priority".to_owned())));
    }

    #[test]
    fn pct_zero_denominator_returns_zero() {
        assert_eq!(pct(5, 0), 0.0);
        assert!((pct(1, 4) - 25.0).abs() < 1e-9);
    }
}
