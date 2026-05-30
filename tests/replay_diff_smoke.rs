//! Integration smoke tests for `replay_diff` binary.
//!
//! Generates real v42 trace files, then invokes the binary as a subprocess and
//! asserts exit codes and stdout snippets.
//!
//! Requires `cargo build --bin replay_diff --features dev` (binary must exist
//! at `target/debug/replay_diff`).

mod common;

use std::io::Write as IoWrite;
use std::path::PathBuf;
use std::process::Command;

use storyforge::combat_engine::{
    DiceRng,
    action::Action,
    state::{CombatState, RoundPhase, UnitId},
    step::step,
    trace::{SCHEMA_VERSION, InitLine, StepLine, post_state_hash_hex, serialize_init, serialize_step},
    TomlContentView,
};

use crate::common::engine_unit::EngineUnitBuilder;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_unit(id: u64) -> storyforge::combat_engine::state::Unit {
    EngineUnitBuilder::new(id).pos(id as i32, 0).build()
}

fn make_init(state: &CombatState, seed: u64) -> InitLine {
    InitLine {
        schema: SCHEMA_VERSION,
        session_id: "smoke_test".to_owned(),
        rng_seed: seed,
        units: state.units().to_vec(),
        next_synthetic_uid: state.next_synthetic_uid(),
        round: state.round,
        phase: state.phase,
        turn_queue: state.turn_queue.clone(),
        content_hash: "blake3:smoke".to_owned(),
    }
}

fn record_trace(state: &CombatState, seed: u64, actions: &[Action]) -> String {
    let content = TomlContentView::empty();
    let init = make_init(state, seed);
    let mut lines = vec![serialize_init(&init).unwrap()];

    let mut s = state.clone();
    let mut rng = DiceRng::with_seed(seed);

    for (idx, action) in actions.iter().enumerate() {
        let (events, ctx) = step(&mut s, action.clone(), &mut rng, &content)
            .unwrap_or_else(|e| panic!("step {idx} failed: {e:?}"));
        let hash = post_state_hash_hex(&s);
        let line = StepLine {
            schema: SCHEMA_VERSION,
            step: idx as u64,
            action: action.clone(),
            events,
            rng_calls: ctx.rng_calls,
            post_state_hash: hash,
        };
        lines.push(serialize_step(&line).unwrap());
    }
    lines.join("\n")
}

fn write_tempfile(content: &str, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("replay_diff_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    path
}

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/replay_diff")
}

fn run_binary(a: &PathBuf, b: &PathBuf) -> std::process::Output {
    Command::new(binary_path())
        .arg(a)
        .arg(b)
        .output()
        .expect("failed to launch replay_diff — run `cargo build --bin replay_diff --features dev` first")
}

fn minimal_state() -> CombatState {
    let u = make_unit(1);
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![UnitId(1)], 0);
    state
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Same trace twice → exit 0, output says "no divergence".
#[test]
fn smoke_identical_traces_exit_0() {
    let state = minimal_state();
    let actions = vec![Action::EndTurn { actor: UnitId(1) }];
    let trace = record_trace(&state, 42, &actions);

    let path_a = write_tempfile(&trace, "smoke_identical_a.jsonl");
    let path_b = write_tempfile(&trace, "smoke_identical_b.jsonl");

    let out = run_binary(&path_a, &path_b);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(0), "expected exit 0\nstdout: {stdout}");
    assert!(
        stdout.contains("no divergence"),
        "expected 'no divergence' in output\nstdout: {stdout}"
    );
}

/// Mutate post_state_hash in step line → exit 2, output mentions DIVERGED.
#[test]
fn smoke_mutated_hash_exit_2() {
    let state = minimal_state();
    let actions = vec![Action::EndTurn { actor: UnitId(1) }];
    let trace_a = record_trace(&state, 42, &actions);

    // Corrupt the last character of the hash in trace_b.
    let trace_b = trace_a.replace("\"post_state_hash\":\"", "\"post_state_hash\":\"x");

    let path_a = write_tempfile(&trace_a, "smoke_mutated_a.jsonl");
    let path_b = write_tempfile(&trace_b, "smoke_mutated_b.jsonl");

    let out = run_binary(&path_a, &path_b);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (divergence)\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("DIVERGED"),
        "expected 'DIVERGED' in output\nstdout: {stdout}"
    );
}

/// Different seeds → init diverges → exit 2.
#[test]
fn smoke_different_seeds_exit_2() {
    let state = minimal_state();
    let actions: Vec<Action> = vec![];
    let trace_a = record_trace(&state, 1, &actions);
    let trace_b = record_trace(&state, 2, &actions);

    let path_a = write_tempfile(&trace_a, "smoke_seed_a.jsonl");
    let path_b = write_tempfile(&trace_b, "smoke_seed_b.jsonl");

    let out = run_binary(&path_a, &path_b);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(2), "expected exit 2\nstdout: {stdout}");
    assert!(
        stdout.contains("DIVERGED") || stdout.contains("rng_seed"),
        "expected divergence report\nstdout: {stdout}"
    );
}
