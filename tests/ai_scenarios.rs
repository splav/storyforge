//! Scenario-based AI regression tests.
//!
//! Walks [`tests/ai_scenarios/snapshots/`] and asserts every captured
//! decision against its overlay file. Each pair is `<name>.jsonl` (one or
//! more log entries extracted from `logs/`) + `<name>.jsonl.expected.toml`
//! (see [`Overlay`](storyforge::combat::ai::replay_assertion) for format).
//!
//! Unlike [`tests/replay_assert.rs`], which spawns the `replay_ai_log`
//! binary, this harness calls [`assert_log_file`] directly. The whole
//! batch runs in a single process — one content load, no subprocess, so
//! 10–15 scenarios finish in well under a second.
//!
//! To add a scenario: drop the JSONL (from a real playtest) and an
//! overlay into `tests/ai_scenarios/snapshots/`, run `cargo test --test
//! ai_scenarios`. See `tests/ai_scenarios/README.md`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use storyforge::combat::ai::influence::InfluenceConfig;
use storyforge::combat::ai::replay::{assert_log_file, default_overlay_path};
use storyforge::combat::ai::replay_assertion::AssertResult;
use storyforge::content::content_view::ContentView;

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ai_scenarios/snapshots")
}

/// Collect `*.jsonl` files in the snapshots directory whose sibling
/// `<name>.jsonl.expected.toml` exists. Ordered by filename for
/// reproducible failure output.
fn discover_pairs(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => panic!("cannot read snapshots dir {}: {e}", dir.display()),
    };
    for entry in entries {
        let path = entry.expect("read_dir entry").path();
        if path.extension() != Some(OsStr::new("jsonl")) {
            continue;
        }
        let overlay = default_overlay_path(&path);
        if !overlay.exists() {
            panic!(
                "orphan snapshot {} — create sibling overlay {}",
                path.display(),
                overlay.display()
            );
        }
        pairs.push((path, overlay));
    }
    pairs.sort();
    pairs
}

#[test]
fn all_ai_scenarios_pass() {
    let dir = snapshots_dir();
    let pairs = discover_pairs(&dir);
    assert!(
        !pairs.is_empty(),
        "no scenarios found under {}",
        dir.display()
    );

    // Content and influence config load once per batch, not per scenario.
    // Scenarios live on top of the global `assets/data` content — if a
    // scenario ever needs a campaign/scenario overlay, thread it here.
    let global = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/data");
    let content = ContentView::load_layered(&global, &global);
    let inf_cfg = InfluenceConfig::default();

    let mut failures: Vec<String> = Vec::new();
    for (jsonl, overlay) in &pairs {
        let outcome = match assert_log_file(jsonl, overlay, &content, &inf_cfg) {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!(
                    "ERROR  {}\n       overlay: {}\n       {e}",
                    jsonl.display(),
                    overlay.display()
                ));
                continue;
            }
        };
        if let AssertResult::Fail(results) = &outcome.result {
            let mut msg = format!(
                "FAIL   {}\n       overlay: {}\n       actual:\n\
                 \x20        decision_kind  = {:?}\n\
                 \x20        intent_kind    = {:?}\n\
                 \x20        cast_ability   = {:?}\n\
                 \x20        cast_target    = {:?}\n\
                 \x20        end_position   = {:?}\n\
                 \x20        primary_effect = {:?}\n",
                jsonl.display(),
                overlay.display(),
                outcome.actual.decision_kind,
                outcome.actual.intent_kind,
                outcome.actual.cast_ability,
                outcome.actual.cast_target,
                outcome.actual.end_position,
                outcome.actual.primary_effect,
            );
            for r in results {
                msg.push_str(&format!(
                    "       variant [{}]: {} field(s) failed\n",
                    r.variant_idx + 1,
                    r.failures.len()
                ));
                for (field, desc) in &r.failures {
                    msg.push_str(&format!("         {field}: {desc}\n"));
                }
            }
            failures.push(msg);
        }
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} ai scenarios failed:\n\n{}",
            failures.len(),
            pairs.len(),
            failures.join("\n")
        );
    }
}
