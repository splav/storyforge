//! Golden baseline guard for the AI scoring pipeline.
//!
//! Runs `replay_ai_log --compare-golden tests/baselines/baseline_v44.jsonl <fixtures>` and
//! fails on any divergence. Behaviour-preserving refactors should keep this at
//! `0 / N diverged`; intentional behaviour changes require recapturing the
//! baseline (see `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump).
//!
//! Skips with a recapture instruction when `tests/baselines/baseline_v44.jsonl` is absent
//! — clones come without a baseline, and we don't want a missing artifact to
//! mask other test failures.

use std::path::{Path, PathBuf};
use std::process::Command;

fn replay_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if !path.join("replay_ai_log").exists() && path.ends_with("deps") {
        path.pop();
    }
    path.push("replay_ai_log");
    path
}

/// Relative to `CARGO_MANIFEST_DIR`. Bump the filename when `SCHEMA_VERSION`
/// bumps; see `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump.
const BASELINE_REL: &str = "tests/baselines/baseline_v44.jsonl";

fn baseline_abs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE_REL)
}

/// Paths are relative to `CARGO_MANIFEST_DIR` and the replay binary is
/// spawned with `current_dir = CARGO_MANIFEST_DIR`, because golden records
/// embed the source `log_path` verbatim — capture/compare must use the
/// same string form (we use the relative form to keep baseline files
/// portable across checkouts).
fn snapshot_logs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ai_scenarios/snapshots");
    let mut out = Vec::new();
    for group in std::fs::read_dir(&root).expect("read snapshots/").flatten() {
        let p = group.path();
        if !p.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&p).expect("read group dir").flatten() {
            let f = file.path();
            if f.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let group_name = p.file_name().unwrap().to_owned();
                let file_name = f.file_name().unwrap().to_owned();
                out.push(
                    PathBuf::from("tests/ai_scenarios/snapshots")
                        .join(group_name)
                        .join(file_name),
                );
            }
        }
    }
    out.sort();
    out
}

#[test]
fn golden_baseline_zero_diff() {
    let baseline = baseline_abs();
    if !baseline.exists() {
        eprintln!(
            "SKIP golden_baseline_zero_diff: {BASELINE_REL} missing.\n\
             Recapture with:\n  \
             cargo run --release --bin replay_ai_log -- --capture-golden \\\n    \
             tests/baselines/baseline_v44.jsonl tests/ai_scenarios/snapshots/*/log.jsonl"
        );
        return;
    }

    let logs = snapshot_logs();
    assert!(
        !logs.is_empty(),
        "no scenario fixtures found under tests/ai_scenarios/snapshots/"
    );

    let out = Command::new(replay_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--compare-golden")
        .arg(BASELINE_REL)
        .args(&logs)
        .output()
        .expect("run replay_ai_log");

    let code = out.status.code();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code,
        Some(0),
        "compare-golden returned exit {code:?}.\n\
         If decisions changed intentionally, recapture per \
         docs/ai/extension-checklist.md § SCHEMA_VERSION bump.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
