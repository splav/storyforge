//! Bridge smoke tests: engine trace writer, replay, and AI log step-range.
//!
//! Covers `EngineTraceWriter` open/write/close round-trip, end-to-end record
//! and in-process replay with byte-equal events and state hashes (gate #14),
//! and the deferred-flush population of `engine_step_range` in AI log entries
//! (gate #15 / phase 6c).

use bevy::prelude::*;

use storyforge::combat::bridge::CombatStateRes;
use storyforge::combat::DiceRngRes;
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;

use super::common;

// ── EngineTraceWriter smoke test (Phase 5 step 5d, gate #11 / #12 / #14) ─────

/// Verifies that `EngineTraceWriter` can open a file, write an `InitLine`
/// with a `session_id`, then write two `StepLine`s, and that the resulting
/// JSONL is parseable with correct field values.
#[test]
fn engine_trace_writer_init_and_step() {
    use combat_engine::action::Action;
    use combat_engine::state::UnitId;
    use combat_engine::trace::{parse_init, parse_step, InitLine, SCHEMA_VERSION};
    use hexx::Hex;
    use std::io::BufRead;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;

    // Use a temp path unique to this test run (epoch-ns suffix).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("engine_trace_smoke_{ts}.jsonl"));

    let mut writer = EngineTraceWriter::default();
    writer.open(&path).expect("open trace file");

    // Write init line.
    let init = InitLine {
        schema: SCHEMA_VERSION,
        session_id: "test_session".to_owned(),
        rng_seed: 0xDEAD_BEEF,
        units: vec![],
        next_synthetic_uid: 0,
        round: 1,
        phase: combat_engine::state::RoundPhase::ActorTurn,
        turn_queue: combat_engine::TurnQueue::default(),
        content_hash: "blake3:test".to_owned(),
    };
    writer.write_init(&init).expect("write init");

    // Write two step lines.
    let action0 = Action::Move {
        actor: UnitId(1),
        path: vec![Hex::new(0, 0), Hex::new(1, 0)],
    };
    let action1 = Action::EndTurn { actor: UnitId(1) };
    writer
        .write_step(&action0, &[], 0, "blake3:hash0".to_owned())
        .expect("write step 0");
    writer
        .write_step(&action1, &[], 0, "blake3:hash1".to_owned())
        .expect("write step 1");
    writer.close();

    // Parse the file back.
    let file = std::fs::File::open(&path).expect("open for read");
    let mut lines = std::io::BufReader::new(file).lines();

    // Line 1: InitLine.
    let line1 = lines.next().expect("line 1 missing").expect("io");
    let parsed_init = parse_init(&line1).expect("parse init");
    assert_eq!(parsed_init.session_id, "test_session");
    assert_eq!(parsed_init.rng_seed, 0xDEAD_BEEF);

    // Line 2: StepLine step=0.
    let line2 = lines.next().expect("line 2 missing").expect("io");
    let parsed_step0 = parse_step(&line2).expect("parse step 0");
    assert_eq!(parsed_step0.step, 0);
    assert!(matches!(parsed_step0.action, Action::Move { .. }));

    // Line 3: StepLine step=1.
    let line3 = lines.next().expect("line 3 missing").expect("io");
    let parsed_step1 = parse_step(&line3).expect("parse step 1");
    assert_eq!(parsed_step1.step, 1);
    assert!(matches!(parsed_step1.action, Action::EndTurn { .. }));

    assert!(lines.next().is_none(), "no extra lines");
    let _ = std::fs::remove_file(&path);
}

// ── Gate #14: end-to-end record + replay via bridge app ───────────────────────

/// End-to-end test: drives actions through the bridge app with the engine trace
/// writer active, then reads the produced `engine.jsonl` and verifies it can be
/// replayed in-process with byte-equal events, rng_calls, and state hashes.
///
/// Gate #14 from Phase 5 §7.
#[test]
fn engine_trace_full_combat_record_replay() {
    use combat_engine::dice::DiceRng;
    use combat_engine::state::CombatState;
    use combat_engine::step::step as engine_step;
    use combat_engine::trace::{parse_init, parse_step, post_state_hash_hex, SCHEMA_VERSION};
    use std::io::BufRead;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;

    // Use a unique temp path.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("engine_trace_e2e_{ts}.jsonl"));

    // ── Build app ────────────────────────────────────────────────────────────
    let mut app = common::apps::bridge::bridge_app();

    let start_hex = hex_from_offset(0, 0);
    let step1_hex = hex_from_offset(1, 0);
    let step2_hex = hex_from_offset(2, 0);

    let actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            common::apps::bridge::bridge_stats(),
            0, // armor
            0, // magic_resist
            6, // speed
            vec![],
            common::apps::bridge::default_equipment(),
        ))
        .id();

    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor, start_hex);

    // Seed engine state.
    common::apps::bridge::bootstrap(&mut app);

    // ── Open the trace writer + write InitLine manually ───────────────────────
    {
        let mut trace_writer = app.world_mut().resource_mut::<EngineTraceWriter>();
        trace_writer.open(&path).expect("open trace file");
    }
    // Write InitLine from the current engine state.
    {
        use combat_engine::trace::{InitLine, SCHEMA_VERSION};
        let rng_seed = app.world().resource::<DiceRngRes>().0.seed();
        let init = {
            let state = &app.world().resource::<CombatStateRes>().0;
            InitLine {
                schema: SCHEMA_VERSION,
                session_id: "e2e_test".to_owned(),
                rng_seed,
                units: state.units().to_vec(),
                next_synthetic_uid: state.next_synthetic_uid(),
                round: state.round,
                phase: state.phase,
                turn_queue: state.turn_queue.clone(),
                content_hash: "blake3:e2e_test".to_owned(),
            }
        };
        app.world_mut()
            .resource_mut::<EngineTraceWriter>()
            .write_init(&init)
            .expect("write init line");
    }

    // ── Drive 3 actions through the bridge ───────────────────────────────────
    // Action 1: Move to step1_hex.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move {
            actor,
            path: vec![step1_hex],
        });
    app.update();

    // Action 2: Move to step2_hex.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move {
            actor,
            path: vec![step2_hex],
        });
    app.update();

    // Action 3: EndTurn.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::EndTurn { actor });
    app.update();

    // Close the trace writer.
    app.world_mut().resource_mut::<EngineTraceWriter>().close();

    // ── Read the produced engine.jsonl ────────────────────────────────────────
    let file = std::fs::File::open(&path).expect("open trace for read");
    let raw_lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect();

    assert!(
        raw_lines.len() >= 2,
        "expected at least InitLine + 1 StepLine, got {}",
        raw_lines.len()
    );

    // ── In-process replay ─────────────────────────────────────────────────────
    let parsed_init = parse_init(&raw_lines[0]).expect("parse init");
    assert_eq!(parsed_init.schema, SCHEMA_VERSION);

    // Reconstruct state from InitLine.
    let mut replay_state = {
        let mut s = CombatState::new(
            parsed_init.units.clone(),
            parsed_init.round,
            parsed_init.phase,
            parsed_init.rng_seed,
        );
        s.set_turn_queue(
            parsed_init.turn_queue.order.clone(),
            parsed_init.turn_queue.index,
        );
        s.set_next_synthetic_uid(parsed_init.next_synthetic_uid);
        s
    };
    let mut replay_rng = DiceRng::with_seed(parsed_init.rng_seed);

    // Use empty content — the bridge test uses default DiceRngRes (ExpectedValue)
    // for determinism, and no ability content is needed for Move/EndTurn.
    use combat_engine::TomlContentView;
    let content = TomlContentView::empty();

    for (idx, line_str) in raw_lines[1..].iter().enumerate() {
        let recorded = parse_step(line_str).unwrap_or_else(|e| panic!("parse step {idx}: {e}"));

        let (live_events, live_ctx) = engine_step(
            &mut replay_state,
            recorded.action.clone(),
            &mut replay_rng,
            &content,
        )
        .unwrap_or_else(|e| panic!("replay step {idx} failed: {e:?}"));

        assert_eq!(live_events, recorded.events, "step {idx}: events diverged");
        assert_eq!(
            live_ctx.rng_calls, recorded.rng_calls,
            "step {idx}: rng_calls diverged (recorded={} live={})",
            recorded.rng_calls, live_ctx.rng_calls
        );
        let live_hash = post_state_hash_hex(&replay_state);
        assert_eq!(
            live_hash, recorded.post_state_hash,
            "step {idx}: post_state_hash diverged"
        );
    }

    let _ = std::fs::remove_file(&path);
}

// ── Gate #15: engine_step_range populated by deferred flush ──────────────────

/// Verifies Phase 6c: `engine_step_range` in AI log entries is populated with
/// the correct step-counter window `[start, end)` by `flush_pending_ai_log_system`.
///
/// Flow:
///   1. Open AiLogger + EngineTraceWriter to temp files.
///   2. Push one pending entry with start_step = 0 (trace counter before dispatch).
///   3. Drive a Move action through the bridge (step counter → 1).
///   4. flush_pending_ai_log_system runs (in the same chain as process_action_system).
///   5. Read the produced ai.jsonl line; assert engine_step_range == [0, 1].
#[test]
fn ai_log_engine_step_range_populated() {
    use std::io::BufRead;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use storyforge::combat::ai::log::{AiLogger, PendingAiLogEntries};

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ai_path = std::env::temp_dir().join(format!("ai_step_range_smoke_{ts}.jsonl"));
    let trace_path = std::env::temp_dir().join(format!("engine_step_range_trace_{ts}.jsonl"));

    let mut app = common::apps::bridge::bridge_app();

    // Spawn a combatant and seed engine state.
    let start_hex = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);
    let actor = common::apps::bridge::spawn_caster(&mut app, start_hex, vec![]);
    common::apps::bridge::bootstrap(&mut app);

    // Open both writers.
    app.world_mut()
        .resource_mut::<EngineTraceWriter>()
        .open(&trace_path)
        .expect("open trace file");
    app.world_mut()
        .resource_mut::<AiLogger>()
        .open(ai_path.clone())
        .expect("open ai log");

    // Verify step counter starts at 0.
    let step_before = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_before, 0, "step counter must start at 0");

    // Build a minimal actor_tick event (mimics what the AI system would push).
    // We push it directly into PendingAiLogEntries with start_step = 0.
    let fake_entry: storyforge::combat::ai::log::ActorTickEvent =
        serde_json::from_value(serde_json::json!({
            "event_type": "actor_tick",
            "schema_version": 36,
            "round": 1,
            "timestamp_ms": 0u64,
            "actor_id": 0u64,
            "actor_name": "test_actor",
            "snapshot": {"units": [], "round": 1},
            "plans": [],
            "decision": {"kind": "end_turn"}
        }))
        .expect("test fixture parses as ActorTickEvent");
    app.world_mut()
        .resource_mut::<PendingAiLogEntries>()
        .entries
        .push((fake_entry, 0));

    // Dispatch Move — process_action_system advances step counter to 1,
    // then flush_pending_ai_log_system writes the entry with range [0, 1).
    common::apps::bridge::write_move(&mut app, actor, vec![target_hex]);
    app.update();

    // Step counter should now be 1 (one Move step was applied).
    let step_after = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_after, 1, "step counter must be 1 after one Move");

    // Close writers.
    app.world_mut().resource_mut::<EngineTraceWriter>().close();
    app.world_mut().resource_mut::<AiLogger>().close();

    // Pending queue must be empty after flush.
    assert!(
        app.world()
            .resource::<PendingAiLogEntries>()
            .entries
            .is_empty(),
        "pending queue must be empty after flush"
    );

    // Read and verify the ai.jsonl line.
    let file = std::fs::File::open(&ai_path).expect("open ai log for read");
    let lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect();

    assert_eq!(lines.len(), 1, "expected exactly 1 actor_tick line");
    let v: serde_json::Value = serde_json::from_str(&lines[0]).expect("parse actor_tick json");

    let range = v
        .get("engine_step_range")
        .expect("engine_step_range must be present");
    let arr = range
        .as_array()
        .expect("engine_step_range must be an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].as_u64().unwrap(), 0, "start_step must be 0");
    assert_eq!(arr[1].as_u64().unwrap(), 1, "end_step must be 1");

    let _ = std::fs::remove_file(&ai_path);
    let _ = std::fs::remove_file(&trace_path);
}
