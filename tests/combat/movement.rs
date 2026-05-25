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
    let mut app = movement_app();

    let hero = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()), "Hero");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    app.world_mut().get_mut::<ActionPoints>(hero).unwrap().movement_points = 1;
    init_engine_state(&mut app);

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
