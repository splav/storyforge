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
    event::Event,
    state::CombatState,
    step::step,
    trace::{parse_init, parse_step, post_state_hash_hex, InitLine, StepLine, SCHEMA_VERSION},
    DiceRng,
};
use storyforge::combat::bridge::build_ecs_content_view;
use storyforge::content::content_view::{ActiveContent, ActiveContentData};

// ── Args ──────────────────────────────────────────────────────────────────────

struct Args {
    path: PathBuf,
    strict_content: bool,
    tolerance: f32,
    /// Override auto-resolved campaign id (directory name under assets/data/campaigns/).
    campaign: Option<String>,
    /// Override auto-resolved scenario id (sub-directory under the campaign dir).
    scenario: Option<String>,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv
        .first()
        .map(|s| s.as_str())
        .unwrap_or("replay_engine_trace");

    let usage = format!(
        "USAGE: {prog} <path> [--strict-content] [--tolerance <eps>] \
         [--campaign <id>] [--scenario <id>]\n\
         \n\
         <path>              Fight folder (contains engine.jsonl) or direct engine.jsonl path.\n\
         --strict-content    Treat content_hash mismatch as fatal error (exit 2).\n\
         --tolerance <eps>   f32 tolerance for Damage event 'raw' field (default 0.0).\n\
         --campaign <id>     Override campaign id (directory name under assets/data/campaigns/).\n\
         --scenario <id>     Override scenario id (sub-directory of the campaign dir).\n\
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
    let mut campaign: Option<String> = None;
    let mut scenario: Option<String> = None;

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
            "--campaign" => {
                campaign = Some(
                    iter.next()
                        .unwrap_or_else(|| {
                            eprintln!("--campaign requires a value");
                            std::process::exit(1);
                        })
                        .clone(),
                );
            }
            "--scenario" => {
                scenario = Some(
                    iter.next()
                        .unwrap_or_else(|| {
                            eprintln!("--scenario requires a value");
                            std::process::exit(1);
                        })
                        .clone(),
                );
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

    Args {
        path,
        strict_content,
        tolerance,
        campaign,
        scenario,
    }
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
    let mut state = CombatState::new(init.units.clone(), init.round, init.phase, init.rng_seed);
    state.set_turn_queue(init.turn_queue.order.clone(), init.turn_queue.index);
    state.set_next_synthetic_uid(init.next_synthetic_uid);
    state
}

// ── Overlay resolution ────────────────────────────────────────────────────────

/// Resolve (campaign_dir, scenario_dir) from a session_id of the form
/// `<timestamp>_<campaign>_<scenario>_<encounter>`.
///
/// The timestamp token (no internal underscores) is stripped first.
/// Then we probe the filesystem under `campaigns_root` to find the longest
/// campaign directory name that is a prefix of the remaining string (followed
/// by `_`). Within that campaign we do the same for scenario directories.
/// Returns `None` if the campaigns_root doesn't exist, no campaign matches,
/// or no scenario matches.
///
/// Example: `"20260609T041509_bell_under_veil_ch3_ch3_theo"` →
///   `(campaigns/bell_under_veil, campaigns/bell_under_veil/ch3)`
fn resolve_overlay(session_id: &str, campaigns_root: &Path) -> Option<(PathBuf, PathBuf)> {
    if !campaigns_root.is_dir() {
        return None;
    }

    // Drop the timestamp prefix (everything up to and including the first '_').
    let after_ts = session_id.split_once('_')?.1;

    // Find the campaign dir whose name is the longest prefix of `after_ts`
    // followed by '_' (or equal to `after_ts` if nothing remains).
    let campaign_name = longest_dir_prefix(after_ts, campaigns_root)?;
    let campaign_dir = campaigns_root.join(&campaign_name);

    // The part after `<campaign_name>_` is `<scenario>_<encounter>`.
    let after_campaign = after_ts
        .strip_prefix(&campaign_name)
        .and_then(|s| s.strip_prefix('_'))
        .unwrap_or("");

    // Find the scenario dir whose name is the longest prefix of `after_campaign`.
    let scenario_name = longest_dir_prefix(after_campaign, &campaign_dir)?;
    let scenario_dir = campaign_dir.join(&scenario_name);

    Some((campaign_dir, scenario_dir))
}

/// Returns the name of the sub-directory of `parent` whose name is the longest
/// prefix of `s` followed by `_` (or exactly equal to `s`).
/// Returns `None` if no such directory exists.
fn longest_dir_prefix(s: &str, parent: &Path) -> Option<String> {
    let mut best: Option<String> = None;
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() {
            continue;
        }
        // Candidate matches if `s` starts with `<name>_` or `s == name`.
        let is_prefix = s == name || s.starts_with(&format!("{name}_"));
        if is_prefix {
            // Take the longest match.
            if best.as_ref().is_none_or(|b| name.len() > b.len()) {
                best = Some(name);
            }
        }
    }
    best
}

// ── Event comparison ──────────────────────────────────────────────────────────

/// Compare two event slices.  When `tolerance == 0.0` this is byte-equal.
/// For `tolerance > 0.0`, the `raw` f32 field inside `UnitDamaged` events
/// may differ by at most `tolerance`; all other fields and all other event
/// variants are compared strictly.
///
/// Forward-compat stub: the engine currently produces no other f32 fields.
/// If that changes, extend this function.
fn events_match_within(recorded: &[Event], live: &[Event], eps: f32) -> Result<(), String> {
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
                    Event::UnitDamaged {
                        target: rt,
                        source: rs,
                        raw: rr,
                        mitigation: rm,
                        pierces: rp,
                        amount: ra,
                    },
                    Event::UnitDamaged {
                        target: lt,
                        source: ls,
                        raw: lr,
                        mitigation: lm,
                        pierces: lp,
                        amount: la,
                    },
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
    let ref_pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

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
        .map_while(Result::ok)
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

    // 4. Load layered content (global + campaign + scenario) and check hash.
    //
    // Overlay dirs are resolved from CLI flags first; if neither is given,
    // auto-resolved from `init.session_id`.  If resolution fails (standalone
    // trace or unknown campaign), we fall back to global-only content by
    // passing nonexistent placeholder dirs — `load_layered` treats missing
    // dirs gracefully (each level is optional).
    let campaigns_root = Path::new("assets/data/campaigns");

    let overlay = match (&args.campaign, &args.scenario) {
        (Some(campaign_id), Some(scenario_id)) => {
            let campaign_dir = campaigns_root.join(campaign_id);
            let scenario_dir = campaign_dir.join(scenario_id);
            Some((campaign_dir, scenario_dir))
        }
        (Some(_), None) | (None, Some(_)) => {
            eprintln!("error: --campaign and --scenario must both be provided when overriding");
            std::process::exit(1);
        }
        (None, None) => resolve_overlay(&init.session_id, campaigns_root),
    };

    let (campaign_dir, scenario_dir) = match overlay {
        Some((c, s)) => {
            println!(
                "info: loading layered content — campaign={} scenario={}",
                c.display(),
                s.display()
            );
            (c, s)
        }
        None => {
            println!("info: standalone/global-only content (no campaign overlay resolved)");
            // Nonexistent paths → load_layered contributes nothing from those layers.
            (
                PathBuf::from("__no_campaign__"),
                PathBuf::from("__no_scenario__"),
            )
        }
    };

    let content_view = ActiveContentData::load_layered(&campaign_dir, &scenario_dir);
    let active = ActiveContent(content_view);
    let ecs_view = build_ecs_content_view(&active);

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

        let (live_events, live_ctx) =
            match step(&mut state, recorded.action.clone(), &mut rng, &ecs_view) {
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
    println!("OK: {n_steps} step(s) replayed — all events, rng_calls, and state hashes match.");
    println!("    session_id={}", init.session_id);
    println!("    final_hash={final_hash}");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `assets/data/campaigns` tree is used for fidelity.
    /// `bell_under_veil/ch3` exists in the repo.
    #[test]
    fn resolve_overlay_bell_under_veil_ch3() {
        let campaigns_root = Path::new("assets/data/campaigns");
        let result = resolve_overlay(
            "20260609T041509_bell_under_veil_ch3_ch3_theo",
            campaigns_root,
        );
        assert!(result.is_some(), "expected Some but got None");
        let (campaign_dir, scenario_dir) = result.unwrap();
        assert_eq!(
            campaign_dir,
            campaigns_root.join("bell_under_veil"),
            "campaign_dir mismatch"
        );
        assert_eq!(
            scenario_dir,
            campaigns_root.join("bell_under_veil").join("ch3"),
            "scenario_dir mismatch"
        );
    }

    #[test]
    fn resolve_overlay_standalone_session_id_returns_none() {
        let campaigns_root = Path::new("assets/data/campaigns");
        // "standalone" is not a campaign directory that exists.
        let result = resolve_overlay("20260101T000000_standalone_enc1", campaigns_root);
        assert!(result.is_none(), "expected None for unknown campaign");
    }

    #[test]
    fn resolve_overlay_unknown_campaign_returns_none() {
        let campaigns_root = Path::new("assets/data/campaigns");
        let result = resolve_overlay(
            "20260101T000000_nonexistent_campaign_sc1_enc1",
            campaigns_root,
        );
        assert!(result.is_none(), "expected None for nonexistent campaign");
    }

    #[test]
    fn load_layered_contains_whisper_from_beyond() {
        // Smoke-test: the campaign layer should expose whisper_from_beyond.
        let campaigns_root = Path::new("assets/data/campaigns");
        let campaign_dir = campaigns_root.join("bell_under_veil");
        let scenario_dir = campaign_dir.join("ch3");
        if !scenario_dir.is_dir() {
            // Skip if tree not present (shouldn't happen in this repo).
            return;
        }
        let cv = ActiveContentData::load_layered(&campaign_dir, &scenario_dir);
        assert!(
            cv.abilities
                .contains_key(&combat_engine::AbilityId("whisper_from_beyond".to_owned())),
            "whisper_from_beyond not found in layered content — campaign layer not loaded?"
        );
    }
}
