//! Phase 6 D6 contract guard: engine-projected ECS components must only be
//! written by `project_state_to_ecs` (and adjacent helpers) inside
//! `src/combat/engine_bridge.rs`. Direct writes from production code outside
//! the bridge break replay determinism — the engine stops being authoritative
//! for state.
//!
//! This test walks `src/` (skipping the AI subtree which uses its own
//! `UnitSnapshot` sim state, not ECS components) and greps for known
//! mutation patterns. Anything outside the allowlist is a violation.
//!
//! # Scope
//!
//! Guarded patterns:
//! - `<x>.hp = <y>`              (Vital.hp / UnitSnapshot.hp)
//! - `<x>.action_points = <y>`   (ActionPoints.action_points)
//! - `<x>.movement_points = <y>` (ActionPoints.movement_points)
//! - `<x>.remaining = <y>`       (Reactions.remaining)
//!
//! # Allowed files
//!
//! - `src/combat/engine_bridge.rs` — the projector itself + phase-transition
//!   helper that preserves the `hp <= max_hp` invariant after a max_hp delta.
//!
//! # Skipped subtrees
//!
//! - `src/combat/ai/` — sim/AI mutates its own `UnitSnapshot`, not ECS.
//!   These mutations are unrelated to engine projection.
//!
//! # False positives
//!
//! The test uses substring matching, not real Rust parsing. If a new file
//! legitimately needs to write a projected field (e.g. a new spawn path),
//! add it to `ALLOWED_FILES` with a one-line justification.

use std::fs;
use std::path::{Path, PathBuf};

/// Files where engine-projected mutations are legitimate.
const ALLOWED_FILES: &[&str] = &[
    // The projector itself + phase_transition's hp/max_hp invariant fix-up.
    "src/combat/engine_bridge.rs",
    // Round-start initiative roll + reaction refill. Runs on
    // `OnEnter(CombatPhase::StartRound)` BEFORE `init_state_from_ecs` reads
    // ECS state into the engine; if removed, the engine would seed reactions
    // at 0 each round. Engine's `start_round` ALSO refills reactions, but the
    // ECS-side refill is load-bearing for the pre-engine seed path.
    // Phase 6 follow-up: could be folded into the engine by ordering
    // start_round → init_state_from_ecs at round boundary.
    "src/combat/turn_order.rs",
];

/// Subtrees skipped entirely (sim state, not ECS-projected).
const SKIPPED_DIRS: &[&str] = &[
    "src/combat/ai/",
];

/// Substrings that indicate a mutation of an engine-projected field.
/// Kept narrow to avoid false positives on field _reads_ or struct literals.
const FORBIDDEN_PATTERNS: &[(&str, &str)] = &[
    (".hp = ", "Vital.hp"),
    (".action_points = ", "ActionPoints.action_points"),
    (".movement_points = ", "ActionPoints.movement_points"),
    (".remaining = ", "Reactions.remaining"),
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_allowed(path: &Path) -> bool {
    let s = path.to_string_lossy();
    ALLOWED_FILES.iter().any(|allowed| s.ends_with(allowed) || s.contains(allowed))
}

fn is_skipped(path: &Path) -> bool {
    let s = path.to_string_lossy();
    SKIPPED_DIRS.iter().any(|skipped| s.contains(skipped))
}

#[test]
fn engine_projected_components_only_written_by_bridge() {
    let mut files = Vec::new();
    walk(Path::new("src"), &mut files);

    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        if is_skipped(file) || is_allowed(file) {
            continue;
        }
        let Ok(content) = fs::read_to_string(file) else { continue };
        for (lineno, line) in content.lines().enumerate() {
            // Skip comments — substring match catches them otherwise.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("*") {
                continue;
            }
            for (pattern, desc) in FORBIDDEN_PATTERNS {
                if line.contains(pattern) {
                    violations.push(format!(
                        "{}:{}: {} mutated outside engine_bridge.rs\n    | {}",
                        file.display(),
                        lineno + 1,
                        desc,
                        line.trim(),
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Engine-projected ECS components must only be written by \
         `project_state_to_ecs` inside `src/combat/engine_bridge.rs`. \
         If the new callsite is legitimate (e.g. a spawn path), add the \
         file to `ALLOWED_FILES` in `tests/projection_isolation.rs` with \
         a one-line justification.\n\nViolations:\n{}",
        violations.join("\n"),
    );
}

#[test]
fn allowed_files_actually_exist() {
    for allowed in ALLOWED_FILES {
        assert!(
            Path::new(allowed).exists(),
            "allowlist entry `{allowed}` does not exist on disk — rename/delete?",
        );
    }
}
