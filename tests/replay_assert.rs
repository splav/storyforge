//! Integration tests for `replay_ai_log --assert`.
//!
//! Uses a real JSONL log from `logs/` and temporary overlay files.
//! Tests exercise: pass/fail exit codes, stdout/stderr content,
//! --assert with explicit overlay path, and vacuous-pass (empty overlay).
//!
//! FIXME(7.5b): all tests `#[ignore]` after schema v27 clean break.
//! Fixture `focus_target_melee_basic/log.jsonl` is v26 — not parseable
//! by new replay_ai_log. Re-enable after rebuilding fixture from a fresh
//! v27 playtest entry (orchestrator does this in a separate commit).

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

/// First entry of this log is plan_id=0, actor=Зверокров Страж,
/// intent=FocusTarget, decision=MoveAndCast (melee_attack, target 12884901551).
///
/// Source: ai_scenarios fixture (stable, не очищается с logs/). Раньше тест
/// указывал на `logs/20260422T...beastblood_raid.jsonl`, но `logs/` ротируется
/// и файл удалялся между playtest'ами. Fixture commit'нут, поведение
/// идентичное (тот же первый entry структурно).
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
#[ignore = "FIXME(7.5b): v26 fixture, needs v27 rebuild after fresh playtest"]
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
