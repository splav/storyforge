use storyforge::game::hex::hex_from_offset;
mod common;

use bevy::prelude::*;

use bevy::ecs::message::Messages;
use common::*;
use storyforge::game::bundles::hero_bundle;
use storyforge::game::components::{
    ActionPoints, ActiveCombatant, ActiveStatus, Mana, StatusEffects,
};
use storyforge::game::messages::{UseAbility, ValidatedAction};
use storyforge::game::resources::HexPositions;

#[test]
fn valid_use_ability_emits_validated_action() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 1);
}

#[test]
fn wrong_actor_use_ability_is_rejected() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let other = app
        .world_mut()
        .spawn((Name::new("Hero2"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(other).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

#[test]
fn no_action_point_use_ability_is_rejected() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<ActionPoints>(actor)
        .unwrap()
        .action_points = 0;

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

// ── Bounds & range ───────────────────────────────────────────────────────────

#[test]
fn out_of_bounds_target_pos_rejects_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(1, 0));

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(99, 99), // out of bounds
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

#[test]
fn out_of_range_rejects_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(3, 0));

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(3, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

#[test]
fn in_range_allows_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(1, 0));

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(1, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 1);
}

// ── Resource costs ───────────────────────────────────────────────────────────

#[test]
fn insufficient_mana_rejects_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((
            Name::new("Mage"),
            hero_bundle(base_stats(), 0, 3, vec!["fireball".into()], test_equipment()),
            Mana::new(4), // fireball costs 5
        ))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(3, 0));

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: "fireball".into(),
            target,
            target_pos: hex_from_offset(3, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

#[test]
fn sufficient_mana_allows_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((
            Name::new("Mage"),
            hero_bundle(base_stats(), 0, 3, vec!["flash".into()], test_equipment()),
            Mana::new(10),
        ))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(2, 0));

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: "flash".into(),
            target,
            target_pos: hex_from_offset(2, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 1);
}

#[test]
fn disoriented_actor_gets_disadvantage_on_ability() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    // Apply the "disoriented" status directly for this test. In real play it
    // would be applied by an ability or aura; here we short-circuit.
    app.world_mut()
        .entity_mut(actor)
        .insert(StatusEffects(vec![ActiveStatus {
            id: "disoriented".into(),
            rounds_remaining: 3,
            applier: actor,
            dot_per_tick: 0,
        }]));
    app.world_mut().entity_mut(actor).insert(ActiveCombatant);

    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(0, 0),
        },
    );
    app.update();

    let msgs = app.world().resource::<Messages<ValidatedAction>>();
    let emitted: Vec<&ValidatedAction> = msgs.iter_current_update_messages().collect();
    assert_eq!(emitted.len(), 1, "validation should still accept the action");
    assert!(
        emitted[0].disadvantage,
        "disoriented actor must roll with disadvantage"
    );
}

#[test]
fn untouched_actor_has_no_disadvantage() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: hex_from_offset(0, 0),
        },
    );
    app.update();

    let msgs = app.world().resource::<Messages<ValidatedAction>>();
    let emitted: Vec<&ValidatedAction> = msgs.iter_current_update_messages().collect();
    assert_eq!(emitted.len(), 1);
    assert!(
        !emitted[0].disadvantage,
        "baseline ability at default range must not be disadvantaged"
    );
}
