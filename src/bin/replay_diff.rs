//! Structured per-step diff of two engine trace (`.jsonl`) files.
//!
//! Compares two traces produced by the engine recorder, step by step, and
//! reports the first point of divergence (if any).
//!
//! USAGE:
//!   replay_diff <path_a.jsonl> <path_b.jsonl>
//!
//! Exit codes:
//!   0 — traces are identical
//!   1 — SCHEMA version mismatch between the two files (fatal)
//!   2 — divergence found (init or step-level)
//!   3 — parse error (malformed JSONL)

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use combat_engine::trace::{parse_init, parse_step, InitLine, StepLine, SCHEMA_VERSION};

// ── Args ──────────────────────────────────────────────────────────────────────

struct Args {
    path_a: PathBuf,
    path_b: PathBuf,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().map(|s| s.as_str()).unwrap_or("replay_diff");

    let usage = format!(
        "USAGE: {prog} <path_a.jsonl> <path_b.jsonl>\n\n\
         Compares two engine trace files step by step and reports the\n\
         first divergence (or confirms they are identical).\n\n\
         Exit codes:\n\
           0 — identical\n\
           1 — SCHEMA mismatch\n\
           2 — divergence found\n\
           3 — parse error"
    );

    if argv.len() != 3 {
        eprintln!("{usage}");
        std::process::exit(1);
    }

    if argv[1] == "--help" || argv[1] == "-h" {
        println!("{usage}");
        std::process::exit(0);
    }

    Args {
        path_a: PathBuf::from(&argv[1]),
        path_b: PathBuf::from(&argv[2]),
    }
}

// ── File loading ──────────────────────────────────────────────────────────────

fn load_lines(path: &PathBuf) -> Vec<String> {
    if !path.exists() {
        eprintln!("error: file not found: {}", path.display());
        std::process::exit(3);
    }
    let file = File::open(path).unwrap_or_else(|e| {
        eprintln!("error: cannot open {}: {e}", path.display());
        std::process::exit(3);
    });
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect()
}

// ── Diff result types ─────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum DiffResult {
    /// Both traces are identical.
    Identical { steps: usize },
    /// Schema versions differ — cannot compare further.
    SchemaMismatch { schema_a: u32, schema_b: u32 },
    /// Init lines differ.
    InitDiverged { detail: String },
    /// A specific step diverges; context printed as a multi-line string.
    StepDiverged { step_idx: usize, detail: String },
    /// One file has more steps than the other after all common steps matched.
    LengthMismatch {
        common_steps: usize,
        extra_in: &'static str,
        extra_count: usize,
    },
}

// ── Core diff logic (extracted for testability) ───────────────────────────────

/// Compare two pre-parsed trace payloads.
///
/// `lines_a` / `lines_b` are raw JSONL lines (first line = InitLine,
/// remaining = StepLines), exactly as loaded from disk.
pub fn diff(lines_a: &[String], lines_b: &[String]) -> DiffResult {
    // ── 1. Parse init lines ────────────────────────────────────────────────
    if lines_a.is_empty() || lines_b.is_empty() {
        return DiffResult::InitDiverged {
            detail: "one or both files are empty".to_string(),
        };
    }

    let init_a: InitLine = match parse_init(&lines_a[0]) {
        Ok(v) => v,
        Err(e) => {
            return DiffResult::InitDiverged {
                detail: format!("A init parse error: {e}"),
            }
        }
    };
    let init_b: InitLine = match parse_init(&lines_b[0]) {
        Ok(v) => v,
        Err(e) => {
            return DiffResult::InitDiverged {
                detail: format!("B init parse error: {e}"),
            }
        }
    };

    // ── 2. Schema check ────────────────────────────────────────────────────
    if init_a.schema != init_b.schema {
        return DiffResult::SchemaMismatch {
            schema_a: init_a.schema,
            schema_b: init_b.schema,
        };
    }

    // ── 3. Init content check ──────────────────────────────────────────────
    if init_a != init_b {
        let mut detail = String::new();
        if init_a.content_hash != init_b.content_hash {
            writeln!(
                detail,
                "  content_hash: A={} B={}",
                init_a.content_hash, init_b.content_hash
            )
            .unwrap();
        }
        if init_a.rng_seed != init_b.rng_seed {
            writeln!(
                detail,
                "  rng_seed: A={} B={}",
                init_a.rng_seed, init_b.rng_seed
            )
            .unwrap();
        }
        if init_a.session_id != init_b.session_id {
            writeln!(
                detail,
                "  session_id: A={} B={}",
                init_a.session_id, init_b.session_id
            )
            .unwrap();
        }
        if init_a.units.len() != init_b.units.len() {
            writeln!(
                detail,
                "  unit count: A={} B={}",
                init_a.units.len(),
                init_b.units.len()
            )
            .unwrap();
        }
        if detail.is_empty() {
            detail.push_str("  (init structs differ — fields not individually listed)");
        }
        return DiffResult::InitDiverged { detail };
    }

    // ── 4. Step-by-step comparison ─────────────────────────────────────────
    let steps_a = &lines_a[1..];
    let steps_b = &lines_b[1..];
    let common = steps_a.len().min(steps_b.len());

    for idx in 0..common {
        let step_a: StepLine = match parse_step(&steps_a[idx]) {
            Ok(s) => s,
            Err(e) => {
                return DiffResult::StepDiverged {
                    step_idx: idx,
                    detail: format!("A parse error at step {idx}: {e}"),
                }
            }
        };
        let step_b: StepLine = match parse_step(&steps_b[idx]) {
            Ok(s) => s,
            Err(e) => {
                return DiffResult::StepDiverged {
                    step_idx: idx,
                    detail: format!("B parse error at step {idx}: {e}"),
                }
            }
        };

        if step_a == step_b {
            continue;
        }

        // Build a detailed divergence report.
        let detail = format_step_divergence(idx, &step_a, &step_b);
        return DiffResult::StepDiverged {
            step_idx: idx,
            detail,
        };
    }

    // ── 5. Length check ────────────────────────────────────────────────────
    if steps_a.len() != steps_b.len() {
        let (extra_in, extra_count) = if steps_a.len() > steps_b.len() {
            ("A", steps_a.len() - steps_b.len())
        } else {
            ("B", steps_b.len() - steps_a.len())
        };
        return DiffResult::LengthMismatch {
            common_steps: common,
            extra_in,
            extra_count,
        };
    }

    DiffResult::Identical { steps: common }
}

// ── Step divergence formatting ────────────────────────────────────────────────

fn format_step_divergence(_idx: usize, a: &StepLine, b: &StepLine) -> String {
    let mut out = String::new();

    // Action
    if a.action != b.action {
        writeln!(out, "  Action:").unwrap();
        writeln!(out, "    A: {:?}", a.action).unwrap();
        writeln!(out, "    B: {:?}", b.action).unwrap();
    }

    // Events
    if a.events != b.events {
        writeln!(
            out,
            "  Events: {} vs {} (count may differ)",
            a.events.len(),
            b.events.len()
        )
        .unwrap();
        let first_diff = a
            .events
            .iter()
            .zip(b.events.iter())
            .position(|(ea, eb)| ea != eb);

        if let Some(diff_idx) = first_diff {
            let context_start = diff_idx.saturating_sub(1);
            let context_end = (diff_idx + 2).min(a.events.len().max(b.events.len()));
            writeln!(out, "  First differing event at index {diff_idx}:").unwrap();
            for ci in context_start..context_end {
                let ea = a
                    .events
                    .get(ci)
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_else(|| "<none>".to_string());
                let eb = b
                    .events
                    .get(ci)
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_else(|| "<none>".to_string());
                let marker = if ci == diff_idx { ">>>" } else { "   " };
                writeln!(out, "  {marker} [{ci}] A: {ea}").unwrap();
                writeln!(out, "  {marker} [{ci}] B: {eb}").unwrap();
            }
            // Show any trailing events in A or B past the common prefix
            let max_len = a.events.len().max(b.events.len());
            if max_len > context_end {
                let remaining = max_len - context_end;
                writeln!(out, "  ... ({remaining} more event(s) not shown)").unwrap();
            }
        } else if a.events.len() != b.events.len() {
            // All shared events match; one has extras
            let (longer, longer_name) = if a.events.len() > b.events.len() {
                (&a.events, "A")
            } else {
                (&b.events, "B")
            };
            let shorter_len = a.events.len().min(b.events.len());
            writeln!(
                out,
                "  First extra event in {longer_name} at index {shorter_len}:"
            )
            .unwrap();
            for (extra_idx, event) in longer.iter().enumerate().skip(shorter_len).take(3) {
                writeln!(out, "    [{extra_idx}] {:?}", event).unwrap();
            }
        }
    }

    // Post-state hash
    if a.post_state_hash != b.post_state_hash {
        writeln!(out, "  Post-state hash:").unwrap();
        writeln!(out, "    A: {}", a.post_state_hash).unwrap();
        writeln!(out, "    B: {}", b.post_state_hash).unwrap();
    }

    // RNG calls
    if a.rng_calls != b.rng_calls {
        writeln!(out, "  RNG calls:").unwrap();
        writeln!(out, "    A: {}", a.rng_calls).unwrap();
        writeln!(out, "    B: {}", b.rng_calls).unwrap();
    }

    out
}

// ── Output rendering ──────────────────────────────────────────────────────────

fn print_and_exit(
    result: DiffResult,
    _init_a: Option<&InitLine>,
    step_summaries: &[(usize, &StepLine)],
) {
    // This function is called from main after streaming output; result carries
    // the final verdict.  We only use init_a / step_summaries for the header.
    match result {
        DiffResult::Identical { steps } => {
            println!("Step 1..{steps}: all identical");
            println!("Total: {steps} steps, no divergence.");
            std::process::exit(0);
        }
        DiffResult::SchemaMismatch { schema_a, schema_b } => {
            eprintln!("SCHEMA MISMATCH: A=v{schema_a} B=v{schema_b} (current={SCHEMA_VERSION})");
            eprintln!("Cannot compare traces with different schemas.");
            std::process::exit(1);
        }
        DiffResult::InitDiverged { detail } => {
            println!("Init: DIVERGED");
            print!("{detail}");
            std::process::exit(2);
        }
        DiffResult::StepDiverged { step_idx, detail } => {
            // Print summary of steps before divergence
            if step_idx > 0 {
                println!("Steps 0..{}: identical", step_idx - 1);
            }
            // Print the diverging step's action label
            let action_label = step_summaries
                .iter()
                .find(|(i, _)| *i == step_idx)
                .map(|(_, s)| format!("{:?}", s.action))
                .unwrap_or_else(|| "?".to_string());
            println!("Step {step_idx} (action={action_label}): DIVERGED");
            print!("{detail}");
            std::process::exit(2);
        }
        DiffResult::LengthMismatch {
            common_steps,
            extra_in,
            extra_count,
        } => {
            println!("Steps 0..{}: identical", common_steps - 1);
            let truncated = if extra_in == "A" { "B" } else { "A" };
            println!(
                "{truncated} truncated after {common_steps} step(s); \
                 {extra_in} has {extra_count} extra step(s)."
            );
            std::process::exit(2);
        }
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args = parse_args();

    let lines_a = load_lines(&args.path_a);
    let lines_b = load_lines(&args.path_b);

    // Quick schema/init header before full diff (gives immediate feedback).
    let init_a: InitLine = match parse_init(lines_a.first().unwrap_or(&String::new())) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: cannot parse A init line: {e}");
            std::process::exit(3);
        }
    };
    let init_b: InitLine = match parse_init(lines_b.first().unwrap_or(&String::new())) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: cannot parse B init line: {e}");
            std::process::exit(3);
        }
    };

    // Schema header
    if init_a.schema != init_b.schema {
        println!(
            "SCHEMA: MISMATCH — A=v{} B=v{} (current=v{SCHEMA_VERSION})",
            init_a.schema, init_b.schema
        );
        eprintln!("Cannot compare traces with different schemas.");
        std::process::exit(1);
    }
    println!("SCHEMA: both v{} ✓", init_a.schema);

    // Init header
    if init_a == init_b {
        println!(
            "Init: identical ✓ (content_hash={}, session_id={})",
            &init_a.content_hash[..8.min(init_a.content_hash.len())],
            init_a.session_id
        );
    }

    // Full diff
    let result = diff(&lines_a, &lines_b);

    // Collect step-action labels for the divergence printer
    let steps_a = &lines_a[1..];
    let step_summaries: Vec<(usize, StepLine)> = steps_a
        .iter()
        .enumerate()
        .filter_map(|(i, l)| parse_step(l).ok().map(|s| (i, s)))
        .collect();
    let step_refs: Vec<(usize, &StepLine)> = step_summaries.iter().map(|(i, s)| (*i, s)).collect();

    // For the identical case, print per-step summary lines (up to 20, then summarise)
    if let DiffResult::Identical { steps } = &result {
        let n = *steps;
        const MAX_PRINT: usize = 20;
        for (idx, line_str) in steps_a.iter().enumerate() {
            if idx >= MAX_PRINT {
                println!("  ... ({} more identical steps)", n - MAX_PRINT);
                break;
            }
            if let Ok(s) = parse_step(line_str) {
                println!(
                    "Step {idx} (action={:?}): identical ({} events, post_hash={}, rng_calls={})",
                    s.action,
                    s.events.len(),
                    &s.post_state_hash[..8.min(s.post_state_hash.len())],
                    s.rng_calls,
                );
            }
        }
    }

    print_and_exit(result, Some(&init_a), &step_refs);
}

// ── Smoke tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid trace JSONL lines.  Schema v42, single step.
    fn make_init_line(seed: u64, session: &str) -> String {
        format!(
            r#"{{"schema":42,"session_id":"{session}","rng_seed":{seed},"units":[],"next_synthetic_uid":0,"round":1,"phase":"pre_round","turn_queue":{{"order":[],"index":0}},"content_hash":"aabbccdd"}}"#
        )
    }

    fn make_step_line(step: u64, hash: &str, rng: u64) -> String {
        format!(
            r#"{{"schema":42,"step":{step},"action":{{"end_turn":{{"actor":1}}}},"events":[],"rng_calls":{rng},"post_state_hash":"{hash}"}}"#
        )
    }

    #[test]
    fn identical_traces_returns_identical() {
        let init = make_init_line(1, "s1");
        let step = make_step_line(0, "deadbeef", 0);
        let lines = vec![init, step];
        assert_eq!(
            diff(&lines, &lines.clone()),
            DiffResult::Identical { steps: 1 }
        );
    }

    #[test]
    fn schema_mismatch_detected() {
        // Build a line with a different schema manually
        let init_a = make_init_line(1, "s1");
        let init_b = init_a.replace("\"schema\":42", "\"schema\":41");
        let lines_a = vec![init_a];
        let lines_b = vec![init_b];
        assert_eq!(
            diff(&lines_a, &lines_b),
            DiffResult::SchemaMismatch {
                schema_a: 42,
                schema_b: 41
            }
        );
    }

    #[test]
    fn step_divergence_on_hash_change() {
        let init = make_init_line(99, "s2");
        let step_a = make_step_line(0, "hash_aaa", 0);
        let step_b = make_step_line(0, "hash_bbb", 0);
        let lines_a = vec![init.clone(), step_a];
        let lines_b = vec![init, step_b];
        match diff(&lines_a, &lines_b) {
            DiffResult::StepDiverged { step_idx, detail } => {
                assert_eq!(step_idx, 0);
                assert!(detail.contains("hash_aaa"), "detail: {detail}");
                assert!(detail.contains("hash_bbb"), "detail: {detail}");
            }
            other => panic!("expected StepDiverged, got {other:?}"),
        }
    }

    #[test]
    fn init_divergence_on_seed_change() {
        let init_a = make_init_line(1, "s1");
        let init_b = make_init_line(2, "s1"); // different seed
        let lines_a = vec![init_a];
        let lines_b = vec![init_b];
        match diff(&lines_a, &lines_b) {
            DiffResult::InitDiverged { detail } => {
                assert!(detail.contains("rng_seed"), "detail: {detail}");
            }
            other => panic!("expected InitDiverged, got {other:?}"),
        }
    }

    #[test]
    fn length_mismatch_reported() {
        let init = make_init_line(1, "s1");
        let step = make_step_line(0, "same", 0);
        let lines_a = vec![init.clone(), step.clone(), make_step_line(1, "same2", 0)];
        let lines_b = vec![init, step];
        assert_eq!(
            diff(&lines_a, &lines_b),
            DiffResult::LengthMismatch {
                common_steps: 1,
                extra_in: "A",
                extra_count: 1
            }
        );
    }

    #[test]
    fn empty_file_returns_error() {
        match diff(&[], &[]) {
            DiffResult::InitDiverged { detail } => {
                assert!(detail.contains("empty"), "detail: {detail}");
            }
            other => panic!("expected InitDiverged, got {other:?}"),
        }
    }
}
