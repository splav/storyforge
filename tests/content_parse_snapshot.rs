//! Regression snapshot of the content parser across ALL shipped content under
//! `assets/data/`. Pins the parsed defs' `Debug` form, catching any parse-path
//! change to an ability/status/weapon/armor — including content no golden AI
//! scenario exercises.
//!
//! Scope = the global content layer only. `unit_templates` are
//! campaign/scenario-layered (none global), so they're excluded.
//!
//! `Debug` (not serde) because engine `AbilityDef`/`EffectDef` don't implement
//! `Serialize`/`PartialEq`; `Debug` is deterministic here (fixed field order,
//! entries sorted by id, Vec fields preserve parse order).
//!
//! Recapture, then review the diff before committing:
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
