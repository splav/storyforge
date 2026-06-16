//! Golden baseline guard for the AI scoring pipeline.
//!
//! Runs `replay_ai_log --compare-golden` against the baseline and fails on any
//! divergence. Intentional behaviour changes require recapturing the baseline
//! (see `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump).
//!
//! Skips with a recapture instruction when the baseline is absent — clones come
//! without one, and a missing artifact shouldn't mask other failures.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Relative to `CARGO_MANIFEST_DIR`. Bump the filename when `SCHEMA_VERSION`
/// bumps; see `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump.
const BASELINE_REL: &str = "tests/baselines/baseline_v46.jsonl";

fn baseline_abs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE_REL)
}

/// Paths relative to `CARGO_MANIFEST_DIR` (the replay binary runs with that as
/// `current_dir`): golden records embed `log_path` verbatim, so capture/compare
/// must use the same relative form to keep baselines portable across checkouts.
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
             tests/baselines/baseline_v46.jsonl tests/ai_scenarios/snapshots/*/log.jsonl"
        );
        return;
    }

    let logs = snapshot_logs();
    assert!(
        !logs.is_empty(),
        "no scenario fixtures found under tests/ai_scenarios/snapshots/"
    );

    let out = Command::new(common::bin::sibling_bin("replay_ai_log"))
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
