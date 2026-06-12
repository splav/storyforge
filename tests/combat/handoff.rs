//! Regression test for the ActiveCombatant multi-entity bug fixed in Phase 4e.
//!
//! Before the fix, `translate_end_turn_events` inserted `ActiveCombatant` on
//! the new actor but never removed it from the old one.  After a mid-round
//! handoff `active_q.single()` would return `Err(MultipleEntities)` and combat
//! would freeze.

use bevy::prelude::*;

use crate::common::{apps::engine::*, fixtures::*};
use storyforge::combat::bridge::{apply_bridge_queues_pre_projection, bootstrap_combat_state};
use storyforge::combat::turn_order::build_turn_order;
use storyforge::game::components::ActiveCombatant;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;

fn spawn_at(
    app: &mut App,
    pos: impl Into<storyforge::game::hex::Hex>,
    bundle: impl Bundle,
    name: &'static str,
) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos.into());
    e
}

/// After a player EndTurn handoff to an enemy, exactly one entity should carry
/// `ActiveCombatant`.  Before the fix both the old and new actor had it,
/// causing `active_q.single()` to panic/fail throughout the pipeline.
#[test]
fn exactly_one_active_combatant_after_mid_round_handoff() {
    let mut app = movement_app();

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let _enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    // Hero is the first actor.
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Sanity: one active combatant before the handoff.
    let before = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(
        before, 1,
        "expected 1 ActiveCombatant before EndTurn, got {before}"
    );

    // Player ends their turn — engine emits TurnEnded + TurnStarted.
    write_message(&mut app, ActionInput::EndTurn { actor: hero });
    app.update();

    let after = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(
        after, 1,
        "exactly one ActiveCombatant after mid-round handoff, got {after}"
    );
}

/// Regression test: when the first actor by initiative is dead at round start,
/// the StartRound chain must skip them and activate the first *alive* actor.
///
/// New design (Chunk 2): `build_turn_order` no longer sets `ActiveCombatant`
/// or skips dead actors — it just sorts by initiative.  The engine's
/// `settle_round_start` (called in `bootstrap_combat_state`) advances the cursor
/// past dead/stunned actors and pushes the settled actor into `insert_active`.
/// `apply_bridge_queues_pre_projection` then drains `insert_active` to set
/// `ActiveCombatant` on the correct entity.
///
/// This test runs the full StartRound chain to verify the end-to-end invariant.
#[test]
fn build_turn_order_skips_dead_first_initiative() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::game::resources::{PresetInitiative, TurnQueue};

    let mut app = movement_app();

    // Enemy has higher initiative via preset so it is sorted first in queue.order.
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 5);

    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );
    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );

    // Mark the enemy dead before the round starts:
    // - Set hp=0 in Vital (engine from_ecs checks this)
    // - Move position to HexCorpses (from_ecs gets dead-unit positions from corpses)
    // - Add Dead component (bridge queries may check this)
    {
        use storyforge::game::resources::HexCorpses;
        app.world_mut()
            .get_mut::<storyforge::game::components::Vital>(enemy)
            .unwrap()
            .hp = 0;
        let pos = app
            .world()
            .resource::<storyforge::game::resources::HexPositions>()
            .get(&enemy)
            .unwrap_or_default();
        app.world_mut()
            .resource_mut::<HexCorpses>()
            .insert(enemy, pos);
        app.world_mut()
            .entity_mut(enemy)
            .insert(storyforge::game::components::Dead);
    }

    // Reset ctx.round: movement_app → enter_await_command transitions through
    // StartRound with no combatants (ctx.round becomes 1). Reset to 0 so the
    // next build_turn_order call sees first_round=true and applies the preset.
    app.world_mut()
        .resource_mut::<storyforge::game::resources::CombatContext>()
        .round = 0;

    // Run the StartRound chain: build_turn_order → bootstrap → drain queues.
    // bootstrap calls settle_round_start which pushes the settled actor into
    // insert_active; apply_bridge_queues_pre_projection drains it to set ActiveCombatant.
    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues failed");

    let queue = app.world().resource::<TurnQueue>();
    // Dead enemy should be first in order (highest initiative).
    assert_eq!(
        queue.order.first().copied(),
        Some(enemy),
        "dead enemy must be first in queue.order (highest initiative)"
    );

    // ActiveCombatant must be on the hero (engine-settled, skipping dead enemy).
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero must carry ActiveCombatant"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "dead enemy must NOT carry ActiveCombatant"
    );
}

// ── B3 regression tests ──────────────────────────────────────────────────────
//
// Each test covers one bug class fixed during the bridge turn-lifecycle work.
// Tests are ordered chronologically by the commit that fixed the bug.

// ── Test 1: frame-ordering at round boundary (B0+B1, commit faaaded) ─────────

/// After a hero exhausts all AP and MP in round 1, they must start round 2 with
/// full resources.
///
/// Pre-B0+B1 the engine refilled AP/MP inside `engine_turn_start_system` (after
/// OnEnter(AwaitCommand)), but `player_command_system` read ECS which still
/// showed the previous round's exhausted values → silent auto-EndTurn.
///
/// Fix: engine cascade `step(EndTurn)` now calls `start_actor_turn` for the
/// incoming actor, and `project_state_to_ecs` propagates the refill before any
/// command system runs.
#[test]
fn actor_with_exhausted_resources_can_act_on_round_2() {
    use storyforge::game::components::{ActionPoints, ActiveCombatant};
    use storyforge::game::resources::{PresetInitiative, TurnQueue};

    let mut app = movement_app();

    // Give hero fixed initiative so they always go first.
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 5);

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    // Wave 3: engine owns turn order via roll_initiative_for_all + reconcile_turn_order
    // in bootstrap_combat_state. Presets above guarantee hero first, enemy second.
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // ── Round 1: hero acts (drains AP/MP via the engine), then ends turn. ─────
    // Set hero's AP=0 / MP=0 directly in ECS to simulate a fully-spent turn.
    // (We don't actually issue Move/Cast — we just drain and end manually.)
    {
        let mut ap = app.world_mut().get_mut::<ActionPoints>(hero).unwrap();
        ap.action_points = 0;
        ap.movement_points = 0;
    }
    write_message(&mut app, ActionInput::EndTurn { actor: hero });
    app.update(); // engine: TurnEnded(hero) → TurnStarted(enemy) + start_actor_turn(enemy)

    // Enemy immediately ends their turn → triggers round-wrap.
    write_message(&mut app, ActionInput::EndTurn { actor: enemy });
    app.update(); // engine: TurnEnded(enemy) → RoundStarted → TurnStarted(hero) + start_actor_turn(hero)

    // One more update to let StartRound → AwaitCommand transition settle
    // (build_turn_order runs, then OnEnter(AwaitCommand) fires).
    app.update();
    app.update();

    // ── Assertions for round 2 ───────────────────────────────────────────────
    // TurnQueue must have a current actor.
    // Exactly one entity must carry ActiveCombatant.
    let active_count = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(
        active_count, 1,
        "exactly one ActiveCombatant expected in round 2, got {active_count}"
    );

    // The current actor (whoever won initiative in round 2) must have full AP/MP.
    // Both hero and enemy had their AP/MP drained before ending their round-1 turns.
    // The engine cascade (B0+B1 fix) must refill the incoming actor via start_actor_turn.
    let queue = app.world().resource::<TurnQueue>();
    let active_entity = queue
        .order
        .get(queue.index)
        .copied()
        .expect("TurnQueue must have a current actor in round 2");

    let ap = app
        .world()
        .get::<ActionPoints>(active_entity)
        .expect("active actor must have ActionPoints");
    assert!(
        ap.action_points > 0,
        "active actor's AP must be refilled at round-2 turn start, got {}",
        ap.action_points
    );
    assert!(
        ap.movement_points > 0,
        "active actor's MP must be refilled at round-2 turn start, got {}",
        ap.movement_points
    );
}

// ── Test 2: double-tick from re-importing engine state (B2, ebde94e) ─────────

/// A self-applied status must tick exactly once per actor-turn start, not extra
/// times from the EndTurn handler or any other double-tick path.
///
/// Mechanism:
/// - `bootstrap_combat_state` primes the first actor's turn by calling
///   `start_actor_turn(hero)`, ticking hero's status 3 → 2.
/// - Hero EndTurn → enemy's start_actor_turn fires (applier check: does NOT
///   tick hero's status). Status stays at 2.
/// - Enemy EndTurn → round wraps → `start_actor_turn(hero)` fires again in the
///   engine cascade. Status ticks 2 → 1.
/// - After project_state_to_ecs the ECS value should be 1 (two ticks total:
///   bootstrap + round-wrap, each exactly once).
///
/// The double-tick regression would produce 0 (an extra tick somewhere).
#[test]
fn status_does_not_tick_twice_per_turn() {
    use storyforge::game::components::{ActiveCombatant, ActiveStatus, StatusEffects};
    use storyforge::game::resources::PresetInitiative;

    let mut app = movement_app();

    // Hero goes first, enemy goes second.
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 5);

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    // Attach a StatusEffects component with a 3-round status.
    // bootstrap primes hero's turn → tick 3→2.
    // One full turn cycle (hero EndTurn + enemy EndTurn) wraps the round →
    // start_actor_turn(hero) fires again → tick 2→1.
    // A spurious extra tick would land at 0.
    app.world_mut()
        .entity_mut(hero)
        .insert(StatusEffects(vec![ActiveStatus {
            id: "test_buff".into(),
            rounds_remaining: 3,
            dot_per_tick: 0,
            applier: Some(hero), // applier == hero → ticks on hero's start_actor_turn
        }]));

    init_engine_state(&mut app); // bootstrap: start_actor_turn(hero) → tick 3→2

    // First update: bootstrap already ran via init_engine_state.
    app.update();

    // Hero ends turn → enemy becomes active.
    // enemy's start_actor_turn does NOT tick hero's status (applier check).
    write_message(&mut app, ActionInput::EndTurn { actor: hero });
    app.update(); // engine: TurnEnded(hero) → TurnStarted(enemy) + start_actor_turn(enemy)

    // Enemy ends turn → round wraps → hero active in round 2.
    // Cascade: start_actor_turn(hero) → status ticks 2 → 1.
    write_message(&mut app, ActionInput::EndTurn { actor: enemy });
    app.update(); // engine: TurnEnded(enemy) → RoundStarted + TurnStarted(hero) inside cascade

    // Let StartRound → AwaitCommand complete.
    app.update();
    app.update();

    // project_state_to_ecs should have written the engine's status value to ECS.
    let status_effects = app.world().get::<StatusEffects>(hero).unwrap();
    let status = status_effects
        .0
        .iter()
        .find(|s| s.id == combat_engine::StatusId::from("test_buff"))
        .expect("test_buff status must still be present");

    assert_eq!(
        status.rounds_remaining, 1,
        "status must have ticked exactly twice (bootstrap + round-wrap cascade), \
         got rounds_remaining={} — extra ticks indicate double-tick regression",
        status.rounds_remaining
    );
}

// ── Test 3: death-mid-action cascade (B5, 4879934) ───────────────────────────

/// Bridge-level integration test: when the **current actor** (enemy) dies during
/// a Move (killed by a hero's AoO), the engine auto-advances the turn queue to
/// the next alive actor (hero).  The ECS must reflect this: `Dead` on the enemy,
/// `ActiveCombatant` on the hero, and no hang.
///
/// There is a pure-engine counterpart in `tests/combat_engine/step.rs`
/// (`current_actor_dies_mid_move_via_aoo_settles_on_next_alive`).  This test
/// exercises the ECS pipeline end-to-end: `process_action_system` →
/// `translate_move_events` → `translate_end_turn_events` → ECS projection.
#[test]
fn current_actor_dies_mid_move_settles_on_next() {
    use storyforge::game::components::{ActiveCombatant, Dead, Vital};

    let mut app = movement_app();

    // Geometry: hero at (4,3), enemy at (3,3) — adjacent in even-r layout.
    // Enemy will move to (1,3) — not adjacent to hero → leaves adjacency → AoO fires.
    let hero_pos = hex_from_offset(4, 3);
    let enemy_pos = hex_from_offset(3, 3);
    let away_hex = hex_from_offset(1, 3);

    let hero = spawn_at(&mut app, hero_pos, test_hero(base_stats()), "Hero");
    let enemy = spawn_at(&mut app, enemy_pos, test_enemy(base_stats()), "Enemy");

    // Enemy is the current (active) actor — give it highest initiative via preset.
    // Wave 3: engine owns turn order so we must use PresetInitiative, not queue.order.
    {
        use storyforge::game::resources::PresetInitiative;
        app.world_mut()
            .resource_mut::<PresetInitiative>()
            .0
            .insert("Enemy".into(), 20);
        app.world_mut()
            .resource_mut::<PresetInitiative>()
            .0
            .insert("Hero".into(), 5);
    }
    app.world_mut().entity_mut(enemy).insert(ActiveCombatant);

    // Make the AoO lethal: enemy has 1 hp; dice roll scripted to 8 (well above 1).
    // Note: roll_initiative_for_all consumes N dice draws (one per unit without a
    // preset). With both units preset, no dice are drawn here, so script([8]) still
    // applies to the AoO roll alone.
    app.world_mut().get_mut::<Vital>(enemy).unwrap().hp = 1;
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[8]);

    init_engine_state(&mut app);

    // Enemy moves away from hero — triggers AoO → lethal.
    write_message(
        &mut app,
        ActionInput::Move {
            actor: enemy,
            path: vec![away_hex],
        },
    );
    app.update();

    // ── Assertions ───────────────────────────────────────────────────────────

    // Enemy must be dead.
    assert!(
        app.world().get::<Dead>(enemy).is_some(),
        "enemy must have Dead marker after lethal AoO"
    );

    // Hero must be active now (engine auto-advanced the turn).
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero must carry ActiveCombatant after enemy dies mid-move (B5 bridge fix)"
    );

    // Enemy must NOT still hold ActiveCombatant.
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "dead enemy must NOT carry ActiveCombatant"
    );
}

// ── Test 4: engine mirror teardown (80ae900 + 0e09215) ───────────────────────

/// `reset_engine_mirrors_on_exit_combat` must clear `CombatStateRes`, `UnitIdMap`,
/// and `PendingPhaseTransitions` so a second combat starts from a clean slate.
///
/// Pre-fix: stale unit data from the first combat survived into the second,
/// causing `project_state_to_ecs` to write dead positions over freshly spawned
/// combatants.
///
/// This test calls the system directly via `run_system_once` — simulating the
/// `OnExit(AppState::Combat)` trigger without the full state-machine overhead.
#[test]
fn combat_2_starts_clean_after_combat_1() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::combat::bridge::{
        reset_engine_mirrors_on_exit_combat, BridgeQueues, CombatStateRes, UnitIdMap,
    };

    let mut app = movement_app();

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Sanity: engine mirrors are populated after init.
    {
        let state = app.world().resource::<CombatStateRes>();
        assert!(
            state.0.units().len() >= 2,
            "engine must have units after init"
        );
        let id_map = app.world().resource::<UnitIdMap>();
        assert!(
            !id_map.entity_to_id.is_empty(),
            "id_map must be populated after init"
        );
    }

    // Seed BridgeQueues.phases with a fake entry.
    {
        use combat_engine::state::UnitId;
        app.world_mut()
            .resource_mut::<BridgeQueues>()
            .phases
            .push((UnitId(999), 0));
    }

    // Simulate OnExit(AppState::Combat) — teardown engine mirrors.
    app.world_mut()
        .run_system_once(reset_engine_mirrors_on_exit_combat)
        .expect("reset_engine_mirrors_on_exit_combat failed");

    // All three mirrors must be empty.
    let state = app.world().resource::<CombatStateRes>();
    assert_eq!(
        state.0.units().len(),
        0,
        "CombatStateRes must be empty after mirror teardown"
    );

    let id_map = app.world().resource::<UnitIdMap>();
    assert!(
        id_map.entity_to_id.is_empty() && id_map.id_to_entity.is_empty(),
        "UnitIdMap must be empty after mirror teardown"
    );

    let queues = app.world().resource::<BridgeQueues>();
    assert!(
        queues.phases.is_empty()
            && queues.deaths.is_empty()
            && queues.animations.is_empty()
            && queues.turn_lifecycle.remove_active.is_empty()
            && queues.turn_lifecycle.insert_active.is_empty()
            && !queues.turn_lifecycle.round_started,
        "BridgeQueues must be fully empty after mirror teardown"
    );

    // Suppress unused-variable warnings for spawned entities.
    let _ = (hero, enemy);
}

// ── Test 4b: bootstrap is fresh after teardown + respawn ─────────────────────

/// After combat 1 → teardown → fresh spawn → bootstrap, the engine must
/// have the new units, no tombstones from combat 1, and accept an action
/// without stale id_map errors.
///
/// Wave 3: engine now owns round-1 order via `roll_initiative_for_all` +
/// `reconcile_turn_order` in `bootstrap_combat_state`.  We use `PresetInitiative`
/// to fix the order so the assertion is deterministic, mirroring the pattern
/// used in `build_turn_order_skips_dead_first_initiative` and
/// `no_stun_active_combatant_is_highest_initiative`.
///
/// Complements `combat_2_starts_clean_after_combat_1` which validates teardown
/// only; this test validates the POST-teardown bootstrap.
#[test]
fn combat_2_bootstraps_fresh_after_combat_1() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::combat::bridge::{
        reset_engine_mirrors_on_exit_combat, CombatStateRes, UnitIdMap,
    };
    use storyforge::game::resources::PresetInitiative;

    let mut app = movement_app();

    // ── Combat 1: spawn 2 units, bootstrap, verify engine mirrors. ────────────
    let hero1 = spawn_at(
        &mut app,
        hex_from_offset(2, 2),
        test_hero(base_stats()),
        "Hero1",
    );
    let enemy1 = spawn_at(
        &mut app,
        hex_from_offset(5, 2),
        test_enemy(base_stats()),
        "Enemy1",
    );
    app.world_mut().entity_mut(hero1).insert(ActiveCombatant);
    init_engine_state(&mut app);
    {
        let state = &app.world().resource::<CombatStateRes>().0;
        assert_eq!(
            state.units().len(),
            2,
            "combat 1: engine must have 2 units after bootstrap"
        );
    }

    // ── Mutate engine state: hero EndTurns once. ──────────────────────────────
    write_message(&mut app, ActionInput::EndTurn { actor: hero1 });
    app.update();

    // ── Teardown: simulate OnExit(AppState::Combat). ──────────────────────────
    app.world_mut()
        .run_system_once(reset_engine_mirrors_on_exit_combat)
        .expect("reset_engine_mirrors_on_exit_combat failed");
    {
        let cs = app.world().resource::<CombatStateRes>();
        assert!(
            cs.0.units().is_empty(),
            "engine units must be cleared by teardown"
        );
        let id_map = app.world().resource::<UnitIdMap>();
        assert!(
            id_map.entity_to_id.is_empty() && id_map.id_to_entity.is_empty(),
            "id_map must be empty after teardown"
        );
    }

    // ── Combat 2: despawn old entities, spawn fresh ones. ─────────────────────
    app.world_mut().entity_mut(hero1).despawn();
    app.world_mut().entity_mut(enemy1).despawn();
    let hero2 = spawn_at(
        &mut app,
        hex_from_offset(1, 1),
        test_hero(base_stats()),
        "Hero2",
    );
    let enemy2 = spawn_at(
        &mut app,
        hex_from_offset(4, 4),
        test_enemy(base_stats()),
        "Enemy2",
    );

    // Use presets to make hero2 first — engine rolls in UnitId order (Wave 2)
    // so without preset the order is non-deterministic across test runs.
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero2".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy2".into(), 5);

    app.world_mut().entity_mut(hero2).insert(ActiveCombatant);

    // ── Bootstrap combat 2 ────────────────────────────────────────────────────
    init_engine_state(&mut app);
    {
        let state = &app.world().resource::<CombatStateRes>().0;
        assert_eq!(
            state.units().len(),
            2,
            "combat 2: exactly 2 units, no stale tombstones"
        );

        // Engine owns the order after Wave 3.  Verify both units are present and
        // hero2 is first (highest initiative via preset).
        let id_map = app.world().resource::<UnitIdMap>();
        let hero2_uid = id_map.get_id(hero2).expect("hero2 must be in id_map");
        let enemy2_uid = id_map.get_id(enemy2).expect("enemy2 must be in id_map");

        assert!(
            state.turn_queue.order.contains(&hero2_uid),
            "combat 2: hero2 must be in engine queue order"
        );
        assert!(
            state.turn_queue.order.contains(&enemy2_uid),
            "combat 2: enemy2 must be in engine queue order"
        );
        assert_eq!(
            state.turn_queue.order.first().copied(),
            Some(hero2_uid),
            "combat 2: hero2 (highest initiative) must be first in engine order"
        );
        assert_eq!(
            state.turn_queue.current(),
            Some(hero2_uid),
            "combat 2: current actor must be hero2 (engine settled on highest initiative)"
        );
    }

    // ── Hero2 takes an action — verify no panic / no stale-id errors. ─────────
    write_message(&mut app, ActionInput::EndTurn { actor: hero2 });
    app.update();
}

// ── Test 5: synthetic UnitId snapshot lookup (B-prime, 1b522d3) ──────────────

/// `BattleSnapshot::entity_for_uid` must not panic for synthetic UnitIds
/// (summons) whose `uid.0` value is above the valid `Entity::from_bits` range.
///
/// Pre-B-prime: callers used `Entity::from_bits(uid.0)` directly, which panics
/// when the UnitId was allocated synthetically by the engine (high-bit set to
/// distinguish from Bevy entity bits).
///
/// Two assertions:
/// 1. `BattleSnapshot::new` (legacy path using `to_bits()` shortcut) silently
///    omits synthetic UIDs — no panic, just `None`.
/// 2. The corrected path (`build_snapshot` via `id_map`) would include them,
///    but we verify the no-panic guarantee here since `build_snapshot` needs full
///    Bevy queries.
#[test]
fn entity_for_uid_lookup_works_for_summoned_units() {
    use combat_engine::state::{CombatState, UnitId};
    use storyforge::combat::ai::world::snapshot::BattleSnapshot;

    // Build a minimal engine state with one regular unit.
    let regular_entity = {
        // Use a throwaway app just to get a valid Entity.
        let mut tmp = App::new();
        tmp.world_mut().spawn(()).id()
    };
    let regular_uid = UnitId(regular_entity.to_bits());

    // Synthetic UnitId: high bit set — NOT a valid Bevy entity bit pattern.
    let synthetic_uid = UnitId(1u64 << 63 | 42);

    // Build a CombatState that contains both units.
    let state = CombatState::default();
    // We cannot easily construct Unit structs directly from tests without engine
    // internals, so we verify the property using BattleSnapshot::new with only
    // the regular unit, then assert the synthetic uid returns None without panic.

    // Verify: entity_for_uid returns None (not panic) for a synthetic UID that
    // was never registered in uid_to_entity.
    let snap = BattleSnapshot::new(state, Default::default());

    // This must NOT panic — pre-fix code called `Entity::from_bits(uid.0)` which
    // would panic for synthetic UIDs.
    let result = snap.entity_for_uid(synthetic_uid);
    assert!(
        result.is_none(),
        "entity_for_uid must return None (not panic) for an unmapped synthetic UnitId"
    );

    // A regular UID (same bit pattern as a valid Bevy entity) is also fine —
    // it's simply absent from the empty snapshot.
    let result2 = snap.entity_for_uid(regular_uid);
    assert!(
        result2.is_none(),
        "entity_for_uid must return None for a uid absent from the snapshot"
    );
}

// ── Test 6: snap.unit(entity) works for summoned units (B-prime audit) ─────────

/// `BattleSnapshot::unit(entity)` must return `Some(UnitView)` for a summoned
/// unit whose engine `UnitId` is synthetic (high bit set — not `entity.to_bits()`).
///
/// Pre-fix: `unit()` computed `UnitId(entity.to_bits())` and looked up that in
/// the engine state, which stored the summon under its synthetic UnitId → `None`.
/// The AI system then wrote `ActionInput::EndTurn` and the summon silently skipped.
///
/// Post-fix: `unit()` uses `entity_to_uid` (populated by `build_snapshot` via
/// `id_map`, or by `new_with_id_map` in tests) — the synthetic-uid summon
/// resolves correctly to `Some(UnitView)`.
#[test]
fn summoned_unit_can_act_in_ai_turn() {
    use bevy::prelude::App;
    use combat_engine::state::{CombatState, RoundPhase, Team, UnitId};
    use hexx::Hex;
    use storyforge::combat::ai::world::cache::{AiCache, UnitAiCache};
    use storyforge::combat::ai::world::snapshot::BattleSnapshot;
    use storyforge::combat::ai::world::tags::AiTags;

    // Allocate two real Bevy entities via a throwaway App.
    let (regular_entity, summon_entity) = {
        let mut tmp = App::new();
        let w = tmp.world_mut();
        (w.spawn(()).id(), w.spawn(()).id())
    };

    // Regular unit: UnitId == entity.to_bits() (the normal shortcut).
    let regular_uid = UnitId(regular_entity.to_bits());

    // Summon: synthetic UnitId with high bit set — NOT a valid Entity bit pattern.
    // This is the kind of UnitId the engine allocates for summoned units.
    let synthetic_uid = UnitId(1u64 << 63 | 99);

    // Sanity: synthetic uid must NOT equal entity.to_bits() — otherwise the test
    // would pass even with the broken shortcut.
    assert_ne!(
        synthetic_uid.0,
        summon_entity.to_bits(),
        "synthetic_uid must differ from summon_entity.to_bits() for this test to be meaningful"
    );

    // Build minimal engine Unit structs.
    let make_engine_unit = |id: UnitId, team: Team| {
        crate::common::engine_unit::EngineUnitBuilder::new(id.0)
            .team(team)
            .pos_hex(Hex::new(0, 0))
            .speed(4)
            .mp(4, 4)
            .build()
    };

    let state = CombatState::new(
        vec![
            make_engine_unit(regular_uid, Team::Player),
            make_engine_unit(synthetic_uid, Team::Enemy),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );

    // Build an AiCache with one entry per entity — required for snap.unit() to
    // resolve (it calls self.cache.unit(entity) which needs a cache row).
    let make_cache_entry = |entity: bevy::prelude::Entity| UnitAiCache {
        entity,
        role: Default::default(),
        threat: 0.0,
        tags: AiTags::empty(),
        max_attack_range: 0,
        aoo_expected_damage: None,
        damage_horizon: vec![],
        crit_fail_effect: Default::default(),
        ai_tuning_override: None,
        abilities: vec![],
        caster_ctx: Default::default(),
        forced_mode: None,
    };
    let cache = AiCache::from_units(vec![
        make_cache_entry(regular_entity),
        make_cache_entry(summon_entity),
    ]);

    // Use the test constructor that accepts an explicit entity↔uid map,
    // mirroring what `build_snapshot` does via `UnitIdMap` in production.
    let snap = BattleSnapshot::new_with_id_map(
        state,
        cache,
        &[
            (regular_entity, regular_uid),
            (summon_entity, synthetic_uid),
        ],
    );

    // Regular unit: must resolve (baseline sanity).
    assert!(
        snap.unit(regular_entity).is_some(),
        "snap.unit must return Some for a regular entity"
    );

    // Summoned unit: must resolve — this was None before the fix.
    assert!(
        snap.unit(summon_entity).is_some(),
        "snap.unit must return Some for a summoned entity with a synthetic UnitId (was None pre-fix)"
    );

    // Symmetry: uid_for_entity / entity_for_uid round-trip.
    assert_eq!(snap.uid_for_entity(summon_entity), Some(synthetic_uid));
    assert_eq!(snap.entity_for_uid(synthetic_uid), Some(summon_entity));
}

// ── Chunk 2 stun-at-round-start tests ────────────────────────────────────────
//
// These tests verify the engine is the sole authority for "whose turn it is":
// a stunned unit that wins initiative is skipped at round start, and
// `ActiveCombatant` is placed on the first alive-and-not-stunned actor.

/// Round 1: a stunned unit with the highest initiative is skipped; the first
/// alive-not-stunned actor gets `ActiveCombatant`; `TurnSkipped` is logged.
#[test]
fn stunned_first_initiative_skipped_on_round_1() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::game::combat_log::{CombatEvent, CombatLog};
    use storyforge::game::components::{ActiveCombatant, ActiveStatus, StatusEffects};
    use storyforge::game::resources::{CombatContext, PresetInitiative, TurnQueue};

    let mut app = movement_app();

    // Enemy wins initiative; hero is second.
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 5);

    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );
    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );

    // Stun the enemy — it wins initiative but must be skipped.
    app.world_mut()
        .get_mut::<StatusEffects>(enemy)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "stunned".into(),
            rounds_remaining: 1,
            applier: None,
            dot_per_tick: 0,
        });

    // Reset ctx.round (movement_app runs StartRound once with no combatants → round=1).
    app.world_mut().resource_mut::<CombatContext>().round = 0;

    // Run the StartRound chain: build_turn_order → bootstrap → drain queues.
    // bootstrap calls settle_round_start which advances past the stunned enemy and
    // pushes the hero into insert_active; apply_bridge_queues_pre_projection sets ActiveCombatant.
    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order");
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues");

    // Verify: enemy sorted first (highest initiative) but NOT active.
    let queue = app.world().resource::<TurnQueue>();
    assert_eq!(
        queue.order.first().copied(),
        Some(enemy),
        "enemy must be first in order"
    );

    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero must be ActiveCombatant"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "stunned enemy must NOT be ActiveCombatant"
    );

    // Exactly one holder.
    let count = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(count, 1, "exactly one ActiveCombatant, got {count}");

    // TurnSkipped must be logged for the enemy.
    let log = app.world().resource::<CombatLog>();
    let skipped = log
        .0
        .iter()
        .any(|e| matches!(e, CombatEvent::TurnSkipped { actor, .. } if *actor == enemy));
    assert!(
        skipped,
        "CombatLog must contain TurnSkipped for stunned enemy"
    );
}

/// Round 2+: after a full round, a stunned unit at position 0 of the new round's
/// order is skipped; hero gets `ActiveCombatant`; combat stays in `AwaitCommand`.
///
/// Setup: enemy=20 initiative (first in order), hero=5.  Enemy is stunned for 2
/// rounds.  Round 1: enemy skipped, hero active.  Hero ends turn → engine sees
/// all actors done (enemy stunned-skip happens in BumpRound) → RoundStarted →
/// StartRound chain.  Round 2: enemy still stunned, must be skipped again.
#[test]
fn stunned_first_initiative_skipped_on_round_2() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::game::components::{ActiveCombatant, ActiveStatus, StatusEffects};
    use storyforge::game::resources::{CombatContext, PresetInitiative};

    let mut app = movement_app();

    // Enemy wins initiative in all rounds (preset consumed round 1, reused round 2+).
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 5);

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    // Stun the enemy for 2 rounds — still stunned when round 2 starts.
    app.world_mut()
        .get_mut::<StatusEffects>(enemy)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "stunned".into(),
            rounds_remaining: 2,
            applier: None,
            dot_per_tick: 0,
        });

    // Reset round counter (movement_app already ran StartRound once with no combatants).
    app.world_mut().resource_mut::<CombatContext>().round = 0;

    // ── Round 1: run StartRound chain manually ────────────────────────────────
    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order r1");
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap r1");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues r1");

    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero active round 1"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "enemy skipped round 1"
    );

    // Hero ends turn → engine skips stunned enemy → BumpRound → RoundStarted →
    // apply_bridge_queues_pre_projection schedules StartRound next frame.
    write_message(&mut app, ActionInput::EndTurn { actor: hero });
    app.update(); // Execute chain: process EndTurn + apply_bridge_queues_pre_projection
    app.update(); // StartRound chain: build_turn_order + bootstrap (exits early) + sync
    app.update(); // AwaitCommand entry — ensure state settled

    // ── Round 2 assertions ────────────────────────────────────────────────────
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero active round 2"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "enemy skipped round 2"
    );

    let count = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(
        count, 1,
        "exactly one ActiveCombatant in round 2, got {count}"
    );
}

/// Regression: a normal no-stun roster → `ActiveCombatant` == highest-initiative
/// alive unit; order unchanged from round to round.
#[test]
fn no_stun_active_combatant_is_highest_initiative() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::game::components::ActiveCombatant;
    use storyforge::game::resources::{CombatContext, PresetInitiative};

    let mut app = movement_app();

    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Hero".into(), 20);
    app.world_mut()
        .resource_mut::<PresetInitiative>()
        .0
        .insert("Enemy".into(), 5);

    let hero = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_hero(base_stats()),
        "Hero",
    );
    let enemy = spawn_at(
        &mut app,
        hex_from_offset(5, 3),
        test_enemy(base_stats()),
        "Enemy",
    );

    // Reset round counter (movement_app → enter_await_command already ran StartRound once).
    app.world_mut().resource_mut::<CombatContext>().round = 0;

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order");
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues");

    // Hero has highest initiative (20) and is alive → must be active.
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero (highest initiative) must be ActiveCombatant"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "enemy must NOT be ActiveCombatant"
    );

    let count = app
        .world_mut()
        .query::<&ActiveCombatant>()
        .iter(app.world())
        .count();
    assert_eq!(count, 1, "exactly one ActiveCombatant, got {count}");
}
