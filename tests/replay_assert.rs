//! Integration tests for `replay_ai_log --assert`.
//!
//! Uses a real JSONL log from `logs/` and temporary overlay files.
//! Tests exercise: pass/fail exit codes, stdout/stderr content,
//! --assert with explicit overlay path, and vacuous-pass (empty overlay).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Path to the binary under test.
fn replay_bin() -> std::path::PathBuf {
    // Cargo places the binary next to the test binary.
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // strip binary name
    // May be in deps/ subdirectory — walk up once if needed.
    if !path.join("replay_ai_log").exists() && path.ends_with("deps") {
        path.pop();
    }
    path.push("replay_ai_log");
    path
}

/// Single-entry v27 fixture. Actor = Зверокров Страж (id 51539607196),
/// intent = FocusTarget, decision = MoveAndCast (melee_attack, target
/// 42949672607). Rebuilt from beastblood_raid playtest 26 апр. 2026
/// after schema v27 clean break.
const LOG_PATH: &str = "tests/ai_scenarios/snapshots/focus_target_melee_basic/log.jsonl";

// ── helpers ──────────────────────────────────────────────────────────────────

/// Create a unique temporary overlay file, write content, return path.
/// File is left for OS cleanup (test runs are short-lived).
fn temp_overlay(content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("sf_assert_test_{pid}_{n}.expected.toml"));
    fs::write(&path, content).expect("write temp overlay");
    path
}

fn run_assert(overlay_content: &str, extra_args: &[&str]) -> std::process::Output {
    let overlay_path = temp_overlay(overlay_content);
    Command::new(replay_bin())
        .arg(LOG_PATH)
        .arg("--assert")
        .arg(&overlay_path)
        .args(extra_args)
        .output()
        .expect("run replay_ai_log")
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Empty overlay → vacuous pass, exit 0.
#[test]
fn empty_overlay_exit_0() {
    let out = run_assert("", &[]);
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS"), "expected PASS in stdout\n{stdout}");
}

/// Correct decision_kind passes (entry is MoveAndCast).
#[test]
fn correct_decision_kind_passes() {
    let out = run_assert(
        r#"
[[expectations]]
decision_kind = ["MoveAndCast"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
}

/// Wrong decision_kind → exit 1, stderr contains FAIL and field name.
#[test]
fn wrong_decision_kind_exit_1() {
    let out = run_assert(
        r#"
[[expectations]]
decision_kind = ["EndTurn"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "expected exit 1\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("FAIL"), "stderr should contain FAIL\n{stderr}");
    assert!(stderr.contains("decision_kind"), "stderr should mention field\n{stderr}");
}

/// any-of: MoveAndCast or CastInPlace → pass (actual is MoveAndCast).
#[test]
fn any_of_decision_kind_passes() {
    let out = run_assert(
        r#"
[[expectations]]
decision_kind = ["CastInPlace", "MoveAndCast"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
}

/// correct ability name → pass.
#[test]
fn correct_cast_ability_passes() {
    let out = run_assert(
        r#"
[[expectations]]
cast_ability = ["melee_attack"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
}

/// wrong ability name → exit 1.
#[test]
fn wrong_cast_ability_exit_1() {
    let out = run_assert(
        r#"
[[expectations]]
cast_ability = ["fireball"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "expected exit 1\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
}

/// Two variants: first wrong, second correct → pass (OR logic).
#[test]
fn two_variants_or_logic_passes() {
    let out = run_assert(
        r#"
[[expectations]]
decision_kind = ["EndTurn"]

[[expectations]]
decision_kind = ["MoveAndCast"]
"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
}

/// Missing overlay file → exit 2.
#[test]
fn missing_overlay_exit_2() {
    let out = Command::new(replay_bin())
        .arg(LOG_PATH)
        .arg("--assert")
        .arg("/nonexistent/path/overlay.expected.toml")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "expected exit 2\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
}

/// --verbose flag prints decision details even on pass.
#[test]
fn verbose_flag_prints_details() {
    let out = run_assert(
        r#"
[[expectations]]
decision_kind = ["MoveAndCast"]
"#,
        &["--verbose"],
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("decision_kind"), "expected verbose output\n{stdout}");
}
