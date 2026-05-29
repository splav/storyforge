/// Engine purity audit (Phase 5 D12).
///
/// Greps every `.rs` source file under `crates/combat_engine/src/` for imports
/// or usages of non-deterministic OS primitives. Any match is a CI blocker
/// because it would silently break replay determinism.
///
/// Forbidden patterns:
/// - `SystemTime`  — wall-clock, non-deterministic.
/// - `std::time::Instant` / `Instant` — monotonic clock, still non-deterministic.
/// - `std::env`    — environment variables vary per host/run.
/// - `std::process` — process ID / Command vary per process.
/// - `thread_local!` — non-deterministic ordering under multi-threaded callers.
///
/// Comment lines (`//`) are excluded so doc-comment references to these names
/// don't trip the guard.

use std::{
    fs,
    path::{Path, PathBuf},
};

const FORBIDDEN: &[&str] = &[
    "SystemTime",
    "std::time::Instant",
    "std::env",
    "std::process",
    "thread_local!",
];

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("read_dir failed");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn engine_src_has_no_forbidden_imports() {
    // Resolve path relative to the crate manifest so the test works regardless
    // of the CWD Cargo picks for test binaries.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("crates/combat_engine/src");

    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(!files.is_empty(), "no .rs files found under {:?}", src_dir);

    let mut violations: Vec<String> = Vec::new();

    for path in &files {
        let content = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {:?}: {}", path, e));

        for (line_no, line) in content.lines().enumerate() {
            // Skip pure comment lines — allows doc-comment references.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }

            for &forbidden in FORBIDDEN {
                if line.contains(forbidden) {
                    violations.push(format!(
                        "  {}:{}: contains {:?}\n    line: {}",
                        path.display(),
                        line_no + 1,
                        forbidden,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Engine purity violation(s) found (D12 — breaks replay determinism):\n{}\n\
         Mitigation: inject timestamps/env via ContentView or step() parameters; \
         never read OS state directly inside the engine.",
        violations.join("\n")
    );
}
