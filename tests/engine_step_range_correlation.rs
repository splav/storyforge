//! Contract test: guards the correlation between `engine_step_range` in
//! `ai_decisions.jsonl` and the actual actor recorded in each `StepLine` of
//! `engine.jsonl`.
//!
//! **What this tests (that no other test does)**:
//! The existing `ai_log_engine_step_range_populated` unit-level test verifies
//! that `engine_step_range` is *set* to the right numeric interval.  This test
//! verifies the *semantic* contract: every `StepLine` whose index falls inside
//! an `ActorTickEvent`'s `[start, end)` range must carry the same actor as the
//! tick event, and ranges must be non-overlapping and together cover all steps.
//!
//! If the pipeline system order changes or the deferred-write logic is
//! refactored, this test will catch the breakage before it silently corrupts
//! replay / mining tooling.

// This test file lives outside the `tests/combat_engine/` directory so that
// Cargo treats it as a separate integration-test binary, keeping its
// `[[test]]` target name distinct from the existing `combat_engine` suite.

use bevy::prelude::*;
use combat_engine::action::Action;
use combat_engine::state::UnitId;
use combat_engine::trace::{parse_init, parse_step};
use std::io::BufRead;
use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
use storyforge::combat::ai::log::{ActorTickEvent, AiLogger, PendingAiLogEntries, SCHEMA_VERSION};
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::bridge::{
    apply_bridge_queues_post_projection, apply_bridge_queues_pre_projection,
    bootstrap_combat_state, entity_to_uid, process_action_system, project_state_to_ecs,
    BridgeQueues, CombatStateRes, UnitIdMap,
};
use storyforge::combat::DiceRngRes;
use storyforge::content::content_view::ActiveContent;
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, HexCorpses, HexPositions, TurnQueue,
    UiDirty,
};
use storyforge::ui::animation::AnimationQueue;
use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

// ── Setup helpers ─────────────────────────────────────────────────────────────

fn correlation_app() -> App {
    use bevy::math::Vec2;
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<CombatEnvironment>()
        .init_resource::<UiDirty>()
        .init_resource::<ActiveContent>()
        .init_resource::<DiceRngRes>()
        .init_resource::<CombatLog>()
        .init_resource::<AnimationQueue>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .init_resource::<BridgeQueues>()
        .init_resource::<EngineTraceWriter>()
        .init_resource::<AiLogger>()
        .init_resource::<PendingAiLogEntries>()
        .init_resource::<AbilityTagCache>()
        .init_resource::<storyforge::game::resources::PresetInitiative>()
        .insert_resource(HexMaterials::default())
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (
                process_action_system,
                apply_bridge_queues_pre_projection,
                project_state_to_ecs,
                apply_bridge_queues_post_projection,
                storyforge::combat::ai::log::flush_pending_ai_log_system,
            )
                .chain(),
        );
    app
}

fn seed_engine(app: &mut App) {
    use bevy::ecs::system::RunSystemOnce;
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
}

fn test_stats() -> storyforge::game::components::CombatStats {
    storyforge::game::components::CombatStats {
        max_hp: 20,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

fn test_equipment() -> storyforge::game::components::Equipment {
    storyforge::game::components::Equipment {
        main_hand: None,
        off_hand: None,
        chest: "mage_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

/// Build a minimal `ActorTickEvent` fixture for a given actor entity and
/// start step, with the correct current `SCHEMA_VERSION`.
fn make_tick_entry(actor: Entity, start_step: u64) -> (ActorTickEvent, u64) {
    let actor_id = actor.to_bits();
    let entry: ActorTickEvent = serde_json::from_value(serde_json::json!({
        "event_type": "actor_tick",
        "schema_version": SCHEMA_VERSION,
        "round": 1,
        "timestamp_ms": 0u64,
        "actor_id": actor_id,
        "actor_name": "test_actor",
        "snapshot": {"units": [], "round": 1},
        "plans": [],
        "decision": {"kind": "end_turn"}
    }))
    .expect("fixture parses as ActorTickEvent");
    (entry, start_step)
}

fn push_action(app: &mut App, msg: ActionInput) {
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(msg);
}

// ── The contract test ─────────────────────────────────────────────────────────

/// Verifies the semantic correlation contract between `engine_step_range` in
/// `ai_decisions.jsonl` and the actor recorded in each `StepLine` of
/// `engine.jsonl`.
///
/// Scenario: two actors perform moves on consecutive frames.
///
/// Frame 1: actor_a does 2 moves → engine steps 0, 1 (range [0, 2))
/// Frame 2: actor_b does 1 move  → engine step  2   (range [2, 3))
///
/// Assertions:
/// A. Every step in actor_a's range [0,2) has Action.actor == uid_a.
/// B. Every step in actor_b's range [2,3) has Action.actor == uid_b.
/// C. Ranges are non-overlapping.
/// D. Union of ranges covers all StepLines (steps 0..3).
/// E. No step from actor_a's uid appears outside its own range.
/// F. No step from actor_b's uid appears outside its own range.
#[test]
fn engine_step_range_correlates_with_action_actor() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ai_path = std::env::temp_dir().join(format!("corr_ai_{ts}.jsonl"));
    let trace_path = std::env::temp_dir().join(format!("corr_engine_{ts}.jsonl"));

    let mut app = correlation_app();

    // ── Spawn two combatants on separate rows so they don't block each other.
    // actor_a: row 0, cols 0→1→2
    // actor_b: row 2, cols 0→1
    let a_start = hex_from_offset(0, 0);
    let a_mid = hex_from_offset(1, 0);
    let a_end = hex_from_offset(2, 0);
    let b_start = hex_from_offset(0, 2);
    let b_end = hex_from_offset(1, 2);

    let actor_a = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0, // armor
            0, // magic_resist
            6, // speed
            vec![],
            test_equipment(),
        ))
        .id();
    let actor_b = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,
            0,
            6,
            vec![],
            test_equipment(),
        ))
        .id();

    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor_a, a_start);
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor_b, b_start);
    seed_engine(&mut app);

    // ── Open both writers ────────────────────────────────────────────────────
    app.world_mut()
        .resource_mut::<EngineTraceWriter>()
        .open(&trace_path)
        .expect("open engine trace");
    // Write InitLine (required for a well-formed engine.jsonl).
    {
        use combat_engine::trace::{InitLine, SCHEMA_VERSION as ENG_SCHEMA};
        let rng_seed = app.world().resource::<DiceRngRes>().0.seed();
        let init = {
            let state = &app.world().resource::<CombatStateRes>().0;
            InitLine {
                schema: ENG_SCHEMA,
                session_id: "corr_test".to_owned(),
                rng_seed,
                units: state.units().to_vec(),
                next_synthetic_uid: state.next_synthetic_uid(),
                round: state.round,
                phase: state.phase,
                turn_queue: state.turn_queue.clone(),
                content_hash: "blake3:corr_test".to_owned(),
            }
        };
        app.world_mut()
            .resource_mut::<EngineTraceWriter>()
            .write_init(&init)
            .expect("write init line");
    }
    app.world_mut()
        .resource_mut::<AiLogger>()
        .open(ai_path.clone())
        .expect("open ai log");

    // Derive engine UnitIds from entities (entity.to_bits() == UnitId.0).
    let uid_a = entity_to_uid(actor_a);
    let uid_b = entity_to_uid(actor_b);

    // ── Frame 1: actor_a makes 2 moves ───────────────────────────────────────
    // Push actor_a's pending entry with start_step = current counter (0).
    let step_before_a = app.world().resource::<EngineTraceWriter>().step_counter();
    app.world_mut()
        .resource_mut::<PendingAiLogEntries>()
        .entries
        .push(make_tick_entry(actor_a, step_before_a));

    // Two move actions in one frame: both are applied by process_action_system
    // before flush_pending_ai_log_system runs.
    push_action(
        &mut app,
        ActionInput::Move {
            actor: actor_a,
            path: vec![a_mid],
        },
    );
    app.update();
    // Flush fires after the first update. Now push second move in next frame
    // (flush has already consumed the pending entry; we need step_counter=1 now).
    // NOTE: we send the second move in a fresh frame so we can observe the
    // counter cleanly.  actor_a's second move is NOT a separate AI tick —
    // it is a sub-action within the same bridge round.  But since we already
    // flushed, we push a separate entry for it as a second tick.
    let step_before_a2 = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_before_a2, 1, "step counter after 1 move = 1");

    // Push second tick for actor_a (start=1, one more move).
    app.world_mut()
        .resource_mut::<PendingAiLogEntries>()
        .entries
        .push(make_tick_entry(actor_a, step_before_a2));
    push_action(
        &mut app,
        ActionInput::Move {
            actor: actor_a,
            path: vec![a_end],
        },
    );
    app.update();

    let step_before_b = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_before_b, 2, "step counter after 2 actor_a moves = 2");

    // ── Frame 2: actor_b makes 1 move ────────────────────────────────────────
    app.world_mut()
        .resource_mut::<PendingAiLogEntries>()
        .entries
        .push(make_tick_entry(actor_b, step_before_b));
    push_action(
        &mut app,
        ActionInput::Move {
            actor: actor_b,
            path: vec![b_end],
        },
    );
    app.update();

    let step_final = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_final, 3, "step counter after 3 total moves = 3");

    // ── Close writers ────────────────────────────────────────────────────────
    app.world_mut().resource_mut::<EngineTraceWriter>().close();
    app.world_mut().resource_mut::<AiLogger>().close();

    // ── Read engine.jsonl ─────────────────────────────────────────────────────
    assert!(
        trace_path.exists(),
        "engine.jsonl missing at {trace_path:?}"
    );
    let engine_lines: Vec<String> = {
        let f = std::fs::File::open(&trace_path).expect("open engine.jsonl");
        std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.is_empty())
            .collect()
    };
    assert!(
        engine_lines.len() >= 2,
        "engine.jsonl must have InitLine + at least 1 StepLine, got {}",
        engine_lines.len()
    );
    // First line is InitLine — skip it.
    let _init = parse_init(&engine_lines[0]).expect("parse InitLine");

    // Parse all StepLines into a map: step_index → UnitId of actor.
    let step_lines: Vec<_> = engine_lines[1..]
        .iter()
        .enumerate()
        .map(|(i, raw)| parse_step(raw).unwrap_or_else(|e| panic!("parse StepLine[{i}]: {e}")))
        .collect();

    // Helper: extract actor UnitId from an Action.
    fn action_actor(action: &Action) -> UnitId {
        match action {
            Action::Move { actor, .. } => *actor,
            Action::Cast { actor, .. } => *actor,
            Action::EndTurn { actor } => *actor,
        }
    }

    // Build step_index → actor map.
    let step_actor: std::collections::HashMap<u64, UnitId> = step_lines
        .iter()
        .map(|sl| (sl.step, action_actor(&sl.action)))
        .collect();

    assert_eq!(
        step_actor.len(),
        3,
        "expected 3 StepLines, got {}",
        step_actor.len()
    );

    // ── Read ai_decisions.jsonl ───────────────────────────────────────────────
    assert!(
        ai_path.exists(),
        "ai_decisions.jsonl missing at {ai_path:?}"
    );
    let ai_lines: Vec<String> = {
        let f = std::fs::File::open(&ai_path).expect("open ai_decisions.jsonl");
        std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.is_empty())
            .collect()
    };
    // 3 actor ticks: actor_a tick1, actor_a tick2, actor_b tick1.
    assert_eq!(
        ai_lines.len(),
        3,
        "expected 3 actor_tick lines, got {}",
        ai_lines.len()
    );

    let ticks: Vec<ActorTickEvent> = ai_lines
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("parse ActorTickEvent[{i}]: {e}"))
        })
        .collect();

    // ── Assertion A/B: steps within each range belong to the right actor ──────
    for (i, tick) in ticks.iter().enumerate() {
        let (start, end) = tick
            .engine_step_range
            .unwrap_or_else(|| panic!("tick[{i}] missing engine_step_range"));
        let expected_uid = UnitId(tick.actor_id);

        for s in start..end {
            let actual_uid = *step_actor.get(&s).unwrap_or_else(|| {
                panic!("tick[{i}] range [{start},{end}): step {s} not found in engine.jsonl")
            });
            assert_eq!(
                actual_uid, expected_uid,
                "tick[{i}] (actor={:?}): step {s} has wrong actor {:?}",
                expected_uid, actual_uid
            );
        }
    }

    // ── Assertion C: ranges are non-overlapping ───────────────────────────────
    let mut ranges: Vec<(u64, u64)> = ticks
        .iter()
        .filter_map(|t| t.engine_step_range)
        .filter(|(s, e)| s < e) // skip zero-length skip-path ranges
        .collect();
    ranges.sort_by_key(|(s, _)| *s);
    for w in ranges.windows(2) {
        let (_, end_prev) = w[0];
        let (start_next, _) = w[1];
        assert!(
            end_prev <= start_next,
            "overlapping ranges: prev ends at {end_prev}, next starts at {start_next}"
        );
    }

    // ── Assertion D: union of ranges covers all step indices ─────────────────
    let all_steps: std::collections::BTreeSet<u64> = step_actor.keys().cloned().collect();
    let covered: std::collections::BTreeSet<u64> = ticks
        .iter()
        .filter_map(|t| t.engine_step_range)
        .flat_map(|(s, e)| s..e)
        .collect();
    assert_eq!(
        covered, all_steps,
        "union of ranges {covered:?} != all step indices {all_steps:?}"
    );

    // ── Assertions E/F: actor does not leak outside its own ranges ────────────
    let steps_for_uid = |uid: UnitId| -> std::collections::BTreeSet<u64> {
        step_actor
            .iter()
            .filter_map(|(s, a)| if *a == uid { Some(*s) } else { None })
            .collect()
    };
    let covered_for_uid = |uid: UnitId| -> std::collections::BTreeSet<u64> {
        ticks
            .iter()
            .filter(|t| UnitId(t.actor_id) == uid)
            .filter_map(|t| t.engine_step_range)
            .flat_map(|(s, e)| s..e)
            .collect()
    };

    for uid in [uid_a, uid_b] {
        let actual = steps_for_uid(uid);
        let claimed = covered_for_uid(uid);
        assert_eq!(
            actual, claimed,
            "actor {uid:?}: steps in engine.jsonl {actual:?} != steps claimed by ranges {claimed:?}"
        );
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    let _ = std::fs::remove_file(&ai_path);
    let _ = std::fs::remove_file(&trace_path);
}
