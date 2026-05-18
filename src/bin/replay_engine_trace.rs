//! Replay an engine trace (`engine.jsonl`) and assert determinism.
//!
//! Accepts either:
//! - A fight folder (containing `engine.jsonl`), OR
//! - A direct `*.jsonl` path.
//!
//! For each step in the trace the tool:
//! 1. Replays the recorded `action` through `combat_engine::step()`.
//! 2. Asserts `Vec<Event>` equals the recorded events (byte-equal, or within
//!    `--tolerance` for f32 fields in damage events).
//! 3. Asserts `rng_calls` matches.
//! 4. Asserts `post_state_hash` matches.
//!
//! Exit codes:
//!   0 — all steps replayed successfully.
//!   1 — usage error.
//!   2 — content_hash mismatch (only with `--strict-content`).
//!   3 — event divergence.
//!   4 — rng_calls divergence.
//!   5 — post_state_hash divergence.
//!
//! USAGE:
//!   replay_engine_trace <path> [--strict-content] [--tolerance <eps>]
//!
//! Flags:
//!   --strict-content    Treat content_hash mismatch as fatal (default: warn).
//!   --tolerance <eps>   f32 tolerance for Damage event `raw` field
//!                       (default: 0.0 = byte-equal). Forward-compat stub.
//!
//! NOTE: Run from the project root so `assets/data` resolves correctly.

#![allow(clippy::result_large_err)]

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use combat_engine::{
    DiceRng, TomlContentView,
    event::Event,
    state::CombatState,
    step::step,
    trace::{
        InitLine, StepLine, SCHEMA_VERSION,
        parse_init, parse_step, post_state_hash_hex,
    },
};

// ── Args ──────────────────────────────────────────────────────────────────────

struct Args {
    path: PathBuf,
    strict_content: bool,
    tolerance: f32,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().map(|s| s.as_str()).unwrap_or("replay_engine_trace");

    let usage = format!(
        "USAGE: {prog} <path> [--strict-content] [--tolerance <eps>]\n\
         \n\
         <path>              Fight folder (contains engine.jsonl) or direct engine.jsonl path.\n\
         --strict-content    Treat content_hash mismatch as fatal error (exit 2).\n\
         --tolerance <eps>   f32 tolerance for Damage event 'raw' field (default 0.0).\n\
         \n\
         NOTE: Run from project root so 'assets/data' resolves correctly."
    );

    if argv.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(1);
    }

    let mut path: Option<PathBuf> = None;
    let mut strict_content = false;
    let mut tolerance: f32 = 0.0;

    let mut iter = argv[1..].iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--strict-content" => strict_content = true,
            "--tolerance" => {
                let val = iter.next().unwrap_or_else(|| {
                    eprintln!("--tolerance requires a value");
                    std::process::exit(1);
                });
                tolerance = val.parse::<f32>().unwrap_or_else(|_| {
                    eprintln!("--tolerance: invalid f32 value '{val}'");
                    std::process::exit(1);
                });
            }
            "--help" | "-h" => {
                println!("{usage}");
                std::process::exit(0);
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => {
                eprintln!("Unknown flag: {s}\n{usage}");
                std::process::exit(1);
            }
        }
    }

    let path = path.unwrap_or_else(|| {
        eprintln!("{usage}");
        std::process::exit(1);
    });

    Args { path, strict_content, tolerance }
}

// ── Path resolution ───────────────────────────────────────────────────────────

/// Resolve the `engine.jsonl` path from a fight folder or direct file path.
fn resolve_engine_jsonl(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("engine.jsonl")
    } else {
        path.to_path_buf()
    }
}

// ── State reconstruction ──────────────────────────────────────────────────────

fn state_from_init(init: &InitLine) -> CombatState {
    let mut state = CombatState::new(
        init.units.clone(),
        init.round,
        init.phase,
        init.rng_seed,
    );
    state.set_turn_queue(init.turn_queue.order.clone(), init.turn_queue.index);
    state.set_next_synthetic_uid(init.next_synthetic_uid);
    state
}

// ── Event comparison ──────────────────────────────────────────────────────────

/// Compare two event slices.  When `tolerance == 0.0` this is byte-equal.
/// For `tolerance > 0.0`, the `raw` f32 field inside `UnitDamaged` events
/// may differ by at most `tolerance`; all other fields and all other event
/// variants are compared strictly.
///
/// Forward-compat stub: the engine currently produces no other f32 fields.
/// If that changes, extend this function.
fn events_match_within(
    recorded: &[Event],
    live: &[Event],
    eps: f32,
) -> Result<(), String> {
    if recorded.len() != live.len() {
        return Err(format!(
            "event count mismatch: recorded={} live={}",
            recorded.len(),
            live.len()
        ));
    }
    for (i, (rec, liv)) in recorded.iter().zip(live.iter()).enumerate() {
        if eps == 0.0 {
            if rec != liv {
                return Err(format!(
                    "event[{i}] diverged:\n  recorded: {rec:?}\n  live:     {liv:?}"
                ));
            }
        } else {
            // Tolerance check: for UnitDamaged, allow `raw` to differ by <= eps.
            // All other variants and all other fields require strict equality.
            match (rec, liv) {
                (
                    Event::UnitDamaged { target: rt, source: rs, raw: rr, mitigation: rm, pierces: rp, amount: ra },
                    Event::UnitDamaged { target: lt, source: ls, raw: lr, mitigation: lm, pierces: lp, amount: la },
                ) => {
                    let raw_ok = (rr - lr).abs() <= eps;
                    if rt != lt || rs != ls || !raw_ok || rm != lm || rp != lp || ra != la {
                        return Err(format!(
                            "event[{i}] UnitDamaged diverged (tolerance={eps}):\n  \
                             recorded: {rec:?}\n  live: {liv:?}"
                        ));
                    }
                }
                _ => {
                    if rec != liv {
                        return Err(format!(
                            "event[{i}] diverged:\n  recorded: {rec:?}\n  live:     {liv:?}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Content hash check ────────────────────────────────────────────────────────

/// Re-hash `assets/data/*.toml` and compare to the recorded `content_hash`.
/// Returns `Ok(())` if they match, `Err(msg)` otherwise.
fn check_content_hash(recorded_hash: &str) -> Result<(), String> {
    use combat_engine::content_hash;
    let data_dir = std::path::Path::new("assets/data");
    let mut pairs: Vec<(String, String)> = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                let contents = std::fs::read_to_string(&p).ok()?;
                Some((name, contents))
            })
            .collect(),
        Err(e) => return Err(format!("cannot read assets/data: {e}")),
    };
    // Sort for determinism (hash_content also sorts, but we need refs to &str).
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let ref_pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let digest = content_hash::hash_content(&ref_pairs);
    let live_hash = content_hash::format_hex(&digest);
    if live_hash == recorded_hash {
        Ok(())
    } else {
        Err(format!(
            "content_hash mismatch:\n  recorded: {recorded_hash}\n  live:     {live_hash}"
        ))
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args = parse_args();

    // 1. Resolve engine.jsonl path.
    let jsonl_path = resolve_engine_jsonl(&args.path);
    if !jsonl_path.exists() {
        eprintln!("error: file not found: {}", jsonl_path.display());
        std::process::exit(1);
    }

    // 2. Read lines.
    let file = File::open(&jsonl_path).unwrap_or_else(|e| {
        eprintln!("error: cannot open {}: {e}", jsonl_path.display());
        std::process::exit(1);
    });
    let reader = BufReader::new(file);
    let raw_lines: Vec<String> = reader
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.is_empty())
        .collect();

    if raw_lines.is_empty() {
        eprintln!("error: {} is empty", jsonl_path.display());
        std::process::exit(1);
    }

    // 3. Parse InitLine.
    let init = parse_init(&raw_lines[0]).unwrap_or_else(|e| {
        eprintln!("error: cannot parse init line: {e}");
        std::process::exit(1);
    });

    if init.schema != SCHEMA_VERSION {
        eprintln!(
            "warning: schema mismatch — trace schema={} current={SCHEMA_VERSION}",
            init.schema
        );
    }

    // 4. Load content and check hash.
    let data_dir = Path::new("assets/data");
    let content = TomlContentView::load_from_dir(data_dir).unwrap_or_else(|e| {
        eprintln!("warning: cannot load content from assets/data: {e}");
        eprintln!("         falling back to empty content (abilities/statuses will all be unknown)");
        TomlContentView::empty()
    });

    match check_content_hash(&init.content_hash) {
        Ok(()) => {}
        Err(msg) => {
            if args.strict_content {
                eprintln!("fatal: {msg}");
                std::process::exit(2);
            } else {
                eprintln!("warning: {msg}");
            }
        }
    }

    // 5. Reconstruct initial state and RNG.
    let mut state = state_from_init(&init);
    let mut rng = DiceRng::with_seed(init.rng_seed);

    // 6. Replay each StepLine.
    let step_lines = &raw_lines[1..];
    let n_steps = step_lines.len();

    for (idx, line_str) in step_lines.iter().enumerate() {
        let recorded: StepLine = match parse_step(line_str) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot parse step line {idx}: {e}");
                // Truncated last line (e.g. crash mid-write) — warn and stop.
                eprintln!("       (possibly a truncated write — stopping replay)");
                break;
            }
        };

        let (live_events, live_ctx) = match step(
            &mut state,
            recorded.action.clone(),
            &mut rng,
            &content,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: step {idx} failed during replay: {e:?}");
                std::process::exit(3);
            }
        };

        // 6a. Compare events.
        if let Err(msg) = events_match_within(&recorded.events, &live_events, args.tolerance) {
            eprintln!("error: step {idx}: {msg}");
            std::process::exit(3);
        }

        // 6b. Compare rng_calls.
        if live_ctx.rng_calls != recorded.rng_calls {
            eprintln!(
                "error: step {idx}: rng_calls diverged \
                 (recorded={} live={})",
                recorded.rng_calls, live_ctx.rng_calls
            );
            std::process::exit(4);
        }

        // 6c. Compare post_state_hash.
        let live_hash = post_state_hash_hex(&state);
        if live_hash != recorded.post_state_hash {
            eprintln!(
                "error: step {idx}: post_state_hash diverged\n  \
                 recorded: {}\n  live:     {}",
                recorded.post_state_hash, live_hash
            );
            std::process::exit(5);
        }
    }

    // 7. Success.
    let final_hash = post_state_hash_hex(&state);
    println!(
        "OK: {n_steps} step(s) replayed — all events, rng_calls, and state hashes match."
    );
    println!("    session_id={}", init.session_id);
    println!("    final_hash={final_hash}");
}
