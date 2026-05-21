/// Regression test for the ActiveCombatant multi-entity bug fixed in Phase 4e.
///
/// Before the fix, `translate_end_turn_events` inserted `ActiveCombatant` on
/// the new actor but never removed it from the old one.  After a mid-round
/// handoff `active_q.single()` would return `Err(MultipleEntities)` and combat
/// would freeze.

use bevy::prelude::*;

use crate::common::*;
use storyforge::game::components::ActiveCombatant;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;

fn spawn_at(app: &mut App, pos: impl Into<storyforge::game::hex::Hex>, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, pos.into());
    e
}

/// After a player EndTurn handoff to an enemy, exactly one entity should carry
/// `ActiveCombatant`.  Before the fix both the old and new actor had it,
/// causing `active_q.single()` to panic/fail throughout the pipeline.
#[test]
fn exactly_one_active_combatant_after_mid_round_handoff() {
    let mut app = movement_app();

    let hero = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()), "Hero");
    let _enemy = spawn_at(&mut app, hex_from_offset(5, 3), test_enemy(base_stats()), "Enemy");

    // Hero is the first actor.
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Sanity: one active combatant before the handoff.
    let before = app.world_mut().query::<&ActiveCombatant>().iter(app.world()).count();
    assert_eq!(before, 1, "expected 1 ActiveCombatant before EndTurn, got {before}");

    // Player ends their turn — engine emits TurnEnded + TurnStarted.
    write_message(&mut app, ActionInput::EndTurn { actor: hero });
    app.update();

    let after = app.world_mut().query::<&ActiveCombatant>().iter(app.world()).count();
    assert_eq!(after, 1, "exactly one ActiveCombatant after mid-round handoff, got {after}");
}

/// Regression test: when the first actor by initiative is dead at round start,
/// `build_turn_order` must skip them and activate the first *alive* actor.
///
/// Before the fix, `queue.index` was hardcoded to 0 and `ActiveCombatant` was
/// inserted on the dead entity, causing all command systems to silently return
/// (dead-actor guard) → infinite hang.
#[test]
fn build_turn_order_skips_dead_first_initiative() {
    use bevy::ecs::system::RunSystemOnce;
    use storyforge::combat::turn_order::build_turn_order;
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

    let enemy = spawn_at(&mut app, hex_from_offset(5, 3), test_enemy(base_stats()), "Enemy");
    let hero  = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()),  "Hero");

    // Mark the enemy dead before the round starts.
    app.world_mut().get_mut::<storyforge::game::components::Vital>(enemy).unwrap().hp = 0;

    // Run build_turn_order directly (avoids needing a full state transition).
    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");

    let queue = app.world().resource::<TurnQueue>();
    // The dead enemy should be first in order (highest initiative) but index must
    // skip it to the living hero.
    assert_ne!(
        queue.order.get(queue.index).copied(),
        Some(enemy),
        "queue.index must not point to the dead enemy"
    );
    assert_eq!(
        queue.order.get(queue.index).copied(),
        Some(hero),
        "queue.index must point to the living hero"
    );

    // ActiveCombatant must be on the hero, not the enemy.
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_some(),
        "hero must carry ActiveCombatant"
    );
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_none(),
        "dead enemy must NOT carry ActiveCombatant"
    );
}
