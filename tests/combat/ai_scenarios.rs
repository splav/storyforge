//! Scenario-based AI regression tests.
//!
//! Layout: `tests/ai_scenarios/snapshots/<group>/log.jsonl` plus one or
//! more `<case>.expected.toml` overlays in the same directory. Each
//! overlay is an independent test case against the group's log; the
//! overlay's `[scope] plan_id` selects which entry. Case filenames
//! typically start with `p<plan_id>_<short_desc>` so the target entry is
//! obvious at a glance.
//!
//! Harness walks `snapshots/` and requires every subdirectory to contain
//! exactly one `*.jsonl` (the group source) plus at least one
//! `*.expected.toml`. Unlike [`tests/replay_assert.rs`] (which spawns the
//! `replay_ai_log` binary), this harness calls [`assert_v28_log_file`]
//! directly — one process, one content load — so 10–15 scenarios finish
//! in well under a second.
//!
//! See `tests/ai_scenarios/README.md` for adding scenarios.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use storyforge::combat::ai::replay::{assert_v28_log_file, AssertResult};
use storyforge::combat::ai::world::influence::InfluenceConfig;
use storyforge::combat::ai::world::snapshot::BattleSnapshot;
use storyforge::content::content_view::ContentView;

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ai_scenarios/snapshots")
}

/// Walk subdirectories of `dir`, pairing each `*.expected.toml` with the
/// single `*.jsonl` that must sit alongside it. Empty-looking dirs,
/// dirs without a JSONL, or dirs with >1 JSONL trigger a panic with the
/// offending path — silent skipping hides scenario mis-setup.
///
/// Returns `(jsonl, overlay, case_name)` triples sorted by path for
/// reproducible failure output. `case_name` is `<group>/<overlay-stem>`
/// and is meant for display only.
fn discover_pairs(dir: &Path) -> Vec<(PathBuf, PathBuf, String)> {
    let mut pairs = Vec::new();
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read snapshots dir {}: {e}", dir.display()));
    for entry in entries {
        let group_dir = entry.expect("read_dir entry").path();
        if !group_dir.is_dir() {
            panic!(
                "unexpected file at snapshots root: {} — scenarios live in subdirs",
                group_dir.display()
            );
        }

        let mut jsonl: Option<PathBuf> = None;
        let mut overlays: Vec<PathBuf> = Vec::new();
        let group_entries = std::fs::read_dir(&group_dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", group_dir.display()));
        for f in group_entries {
            let p = f.expect("read_dir entry").path();
            if !p.is_file() {
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".expected.toml") {
                overlays.push(p);
            } else if p.extension() == Some(OsStr::new("jsonl")) {
                if let Some(existing) = &jsonl {
                    panic!(
                        "group {} has multiple JSONL files ({} and {}) — one per group",
                        group_dir.display(),
                        existing.display(),
                        p.display()
                    );
                }
                jsonl = Some(p);
            }
        }

        let jsonl = jsonl
            .unwrap_or_else(|| panic!("group {} has no *.jsonl source log", group_dir.display()));
        if overlays.is_empty() {
            panic!("group {} has no *.expected.toml cases", group_dir.display());
        }

        let group_name = group_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        for overlay in overlays {
            let case_stem = overlay
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".expected.toml"))
                .unwrap_or("?")
                .to_string();
            let case_name = format!("{group_name}/{case_stem}");
            pairs.push((jsonl.clone(), overlay, case_name));
        }
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

    eprintln!("discovered {} ai scenario case(s)", pairs.len());
    let mut failures: Vec<String> = Vec::new();
    for (jsonl, overlay, case_name) in &pairs {
        let outcome = match assert_v28_log_file(jsonl, overlay, &content, &inf_cfg) {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!(
                    "ERROR  {case_name}\n       log:     {}\n       overlay: {}\n       {e}",
                    jsonl.display(),
                    overlay.display()
                ));
                continue;
            }
        };
        if let AssertResult::Fail(results) = &outcome.result {
            let mut msg = format!(
                "FAIL   {case_name}\n       log:     {}\n       overlay: {}\n       actual:\n\
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

// ---------------------------------------------------------------------------
// Fixture enrichment (run once, before U5/D-final drops legacy fields)
// ---------------------------------------------------------------------------

/// Round-trip each JSONL line's `snapshot` field through `BattleSnapshot`
/// serde, writing back the enriched JSON (adds `state` + `cache` that
/// `rebuild_index` back-fills from `units`).
///
/// Run once with:
/// ```
/// cargo test --test combat enrich_ai_scenarios -- --ignored --exact
/// ```
/// After this, every fixture line's `snapshot` object will carry all four
/// fields (`units`, `round`, `state`, `cache`).  U5/D-final can then safely
/// drop `units`/`round` from `BattleSnapshot`; serde's default behaviour
/// ignores the now-unknown keys in old raw captures.
#[test]
#[ignore]
fn enrich_ai_scenarios() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files: &[&str] = &[
        "tests/ai_scenarios/snapshots/bell_crypt/log.jsonl",
        "tests/ai_scenarios/snapshots/continuation_actor_hp_drop_relevant/log.jsonl",
        "tests/ai_scenarios/snapshots/continuation_cosmetic_rage_tick_no_replan/log.jsonl",
        "tests/ai_scenarios/snapshots/continuation_setup_aoe_two_ticks/log.jsonl",
        "tests/ai_scenarios/snapshots/continuation_target_dies_replan/log.jsonl",
        "tests/ai_scenarios/snapshots/continuation_ttl_expires/log.jsonl",
        "tests/ai_scenarios/snapshots/focus_target_melee_basic/log.jsonl",
        "tests/ai_scenarios/snapshots/road_bridge/log.jsonl",
        "tests/baselines/baseline_v44.jsonl",
    ];
    for rel in files {
        let path = root.join(rel);
        enrich_jsonl_file(&path);
        eprintln!("enriched {rel}");
    }
}

fn enrich_jsonl_file(path: &Path) {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    let enriched: Vec<String> = content
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            let mut obj: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("bad JSON in {}: {e}\nline: {line}", path.display()));

            if let Some(snap_val) = obj.get("snapshot").cloned() {
                // Deserialize triggers rebuild_index, back-filling `state` and
                // `cache` from `units` when those fields are absent (old schema).
                let snap: BattleSnapshot = serde_json::from_value(snap_val).unwrap_or_else(|e| {
                    panic!("snapshot deserialize failed in {}: {e}", path.display())
                });
                let enriched_snap = serde_json::to_value(&snap).unwrap_or_else(|e| {
                    panic!("snapshot serialize failed in {}: {e}", path.display())
                });
                obj.as_object_mut()
                    .expect("log line must be a JSON object")
                    .insert("snapshot".to_string(), enriched_snap);
            }

            serde_json::to_string(&obj)
                .unwrap_or_else(|e| panic!("re-serialize failed in {}: {e}", path.display()))
        })
        .collect();

    // Preserve trailing newline if original had one.
    let mut out = enriched.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, &out).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
