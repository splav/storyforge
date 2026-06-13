//! Regression snapshot of the app content parser across ALL shipped content
//! under `assets/data/`.
//!
//! This is the forward-looking guard that replaces the per-id cross-check
//! `toml_content_view_parity.rs` once gave: with the duplicate engine parser
//! (`TomlContentView::load_from_dir`) deleted, there is no second parser to
//! diff against, so instead we pin the parsed defs themselves (their `Debug`
//! form). A future change to the parse path that alters any ability, status,
//! weapon, or armor is caught here — including content that no golden AI
//! scenario ever exercises (golden only covers used content).
//!
//! Scope = the global content layer (`assets/data`), matching the old parity
//! test's source. `unit_templates` are campaign/scenario-layered (the global
//! layer has none), so they aren't part of this fingerprint.
//!
//! `Debug` (not serde) because engine `AbilityDef`/`EffectDef` deliberately do
//! not implement `Serialize`/`PartialEq`; `Debug` is derived on every parsed
//! type and is deterministic (struct field order is fixed; we sort entries by
//! id, and Vec fields preserve parse order).
//!
//! Recapture after an intentional content or parser change, then review the
//! diff before committing:
//!   UPDATE_CONTENT_SNAPSHOT=1 cargo test --features dev --test content_parse_snapshot

use std::fmt::Write as _;
use std::path::Path;

use storyforge::content::content_view::ActiveContentData;

/// Relative to `CARGO_MANIFEST_DIR`.
const SNAPSHOT_REL: &str = "tests/snapshots/content_parse.snap";

/// Render the full parsed-content fingerprint. Entries are sorted by their
/// `Debug`-rendered id so the output is independent of `HashMap` iteration
/// order.
fn render() -> String {
    let data_dir = Path::new("assets/data");
    let content = ActiveContentData::load_layered(data_dir, data_dir);
    let mut out = String::new();

    macro_rules! section {
        ($title:expr, $map:expr) => {{
            let mut entries: Vec<(String, String)> = $map
                .iter()
                .map(|(id, def)| (format!("{id:?}"), format!("{def:#?}")))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            writeln!(out, "## {} ({})", $title, entries.len()).unwrap();
            for (k, v) in entries {
                writeln!(out, "{k} => {v}").unwrap();
            }
            writeln!(out).unwrap();
        }};
    }

    section!("abilities", content.abilities);
    section!("statuses", content.statuses);
    section!("weapons", content.weapons);
    section!("armor", content.armor);
    out
}

#[test]
fn content_parse_snapshot_matches() {
    let actual = render();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_REL);

    if std::env::var_os("UPDATE_CONTENT_SNAPSHOT").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create snapshots dir");
        std::fs::write(&path, &actual).expect("write snapshot");
        eprintln!("content snapshot updated: {SNAPSHOT_REL}");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {SNAPSHOT_REL}.\nRecapture with:\n  \
             UPDATE_CONTENT_SNAPSHOT=1 cargo test --features dev \
             --test content_parse_snapshot"
        )
    });

    assert_eq!(
        actual, expected,
        "content parse snapshot drift.\nIf this change is intentional, recapture with:\n  \
         UPDATE_CONTENT_SNAPSHOT=1 cargo test --features dev --test content_parse_snapshot\n\
         then review the diff in {SNAPSHOT_REL} before committing."
    );
}
