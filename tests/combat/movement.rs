use bevy::prelude::*;

use crate::common::{fixtures::*, apps::engine::*};
use storyforge::game::components::{ActionPoints, ActiveCombatant};
use storyforge::game::hex::{hex_from_offset, Hex};
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;

fn spawn_at(app: &mut App, pos: Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, pos);
    e
}

#[test]
fn partial_move_subtracts_pool_and_allows_second_move() {
    let mut app = movement_app();

    let hero = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()), "Hero");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    app.world_mut().get_mut::<ActionPoints>(hero).unwrap().movement_points = 3;
    init_engine_state(&mut app);

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![hex_from_offset(2, 3)] });
    app.update();
    assert_eq!(
        app.world().get::<ActionPoints>(hero).unwrap().movement_points,
        2,
        "one hex walked → pool reduced by 1",
    );
    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        Some(hex_from_offset(2, 3)),
    );

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![hex_from_offset(1, 3)] });
    app.update();
    assert_eq!(
        app.world().get::<ActionPoints>(hero).unwrap().movement_points,
        1,
    );
    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        Some(hex_from_offset(1, 3)),
    );
}

#[test]
fn move_rejected_when_path_longer_than_pool() {
    use storyforge::combat::engine_bridge::CombatStateRes;

    let mut app = movement_app();

    let hero = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()), "Hero");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Wave 3: start_actor_turn in settle_round_start refills MP to speed.
    // Set the engine pool to 1 AFTER bootstrap so the engine is the constraint
    // (the engine is authoritative for pool values once bootstrap completes).
    {
        let id_map = app.world().resource::<storyforge::combat::engine_bridge::UnitIdMap>();
        let uid = id_map.get_id(hero).expect("hero must be in id_map");
        let mut cs = app.world_mut().resource_mut::<CombatStateRes>();
        if let Some(u) = cs.0.unit_mut(uid) {
            use combat_engine::PoolKind;
            if let Some((cur, _max)) = u.pools[PoolKind::Mp].as_mut() {
                *cur = 1;
            }
        }
    }

    // Path of length 2, but only 1 point in the pool.
    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![hex_from_offset(2, 3), hex_from_offset(1, 3)],
        },
    );
    app.update();

    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        Some(hex_from_offset(3, 3)),
        "movement rejected — hero did not move",
    );
    assert_eq!(
        app.world().get::<ActionPoints>(hero).unwrap().movement_points,
        1,
        "pool untouched when path rejected",
    );
}

/// A unit can path through an ally's hex but cannot stop on it.
/// Regression guard: the two-layer spatial model (HexPositions / HexCorpses)
/// must not break passthrough semantics for living-unit paths.
#[test]
fn can_pass_through_ally_cannot_stop() {
    let mut app = movement_app();

    // Hero at (3,3), ally at (2,3) — hero wants to reach (1,3) passing through ally.
    let hero  = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()),  "Hero");
    let _ally = spawn_at(&mut app, hex_from_offset(2, 3), test_hero(base_stats()),  "Ally");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    app.world_mut().get_mut::<ActionPoints>(hero).unwrap().movement_points = 3;
    init_engine_state(&mut app);

    // Path passes through ally hex (2,3) and stops at (1,3).
    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![hex_from_offset(2, 3), hex_from_offset(1, 3)],
        },
    );
    app.update();

    let positions = app.world().resource::<HexPositions>();
    assert_eq!(
        positions.get(&hero),
        Some(hex_from_offset(1, 3)),
        "hero must reach destination past ally",
    );
    assert_eq!(
        positions.get(&_ally),
        Some(hex_from_offset(2, 3)),
        "ally must not be displaced",
    );
}
