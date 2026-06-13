//! Step 3 unit tests: `CombatState::from_ecs` + `UnitIdMap`.
//!
//! Round-trip test: build a 10-unit Bevy world, call `from_ecs`, assert that
//! every unit's pos/hp/movement_points/reactions/statuses match the ECS values.

use bevy::prelude::*;

use storyforge::combat::bridge::{from_ecs, UnitIdMap};
use storyforge::combat_engine::state::{CombatState, Team};
use storyforge::content::content_view::ActiveContent;
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::components::{
    ActionPoints, CombatStats, Combatant, Dead, Energy, Equipment, Faction, Mana, Rage, Reactions,
    Speed, StatusEffects, Team as EcsTeam, TemplateRef, Vital,
};
use storyforge::game::hex::{hex_from_offset, Hex};
use storyforge::game::resources::{HexCorpses, HexPositions};

/// Run `from_ecs` against `world` via a one-shot `SystemState`.
/// Centralised here so the chunky query tuple lives in one place.
#[allow(clippy::type_complexity)]
fn run_from_ecs(world: &mut World, round: u32, id_map: &mut UnitIdMap) -> CombatState {
    let mut ss: bevy::ecs::system::SystemState<(
        Query<
            (
                Entity,
                &Vital,
                Option<&Speed>,
                Option<&ActionPoints>,
                Option<&Reactions>,
                &Faction,
                Option<&StatusEffects>,
                Option<&Rage>,
                Option<&Mana>,
                Option<&Energy>,
                Option<&TemplateRef>,
            ),
            With<Combatant>,
        >,
        Res<HexPositions>,
        Res<HexCorpses>,
        Res<ActiveContent>,
    )> = bevy::ecs::system::SystemState::new(world);
    let (combatants, positions, corpses, active_content) = ss.get(world);
    from_ecs(
        &combatants,
        &positions,
        &corpses,
        round,
        id_map,
        &active_content,
    )
}

fn minimal_stats() -> CombatStats {
    CombatStats {
        max_hp: 20,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 5,
        charisma: 5,
    }
}

fn minimal_equipment() -> Equipment {
    Equipment {
        main_hand: None,
        off_hand: None,
        chest: "cloth_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

/// Spawn a combatant at `pos` with overrides applied via `f`.
fn spawn_unit(
    world: &mut World,
    positions: &mut HexPositions,
    pos: Hex,
    bundle: impl Bundle,
) -> Entity {
    let e = world.spawn(bundle).id();
    positions.insert(e, pos);
    e
}

/// Build a 10-unit world: 5 heroes + 5 enemies, each at a distinct position.
/// Returns (entities, positions used).
fn build_10_unit_world(world: &mut World) -> (Vec<Entity>, Vec<Hex>) {
    let mut positions = HexPositions::default();
    let mut entities = Vec::new();
    let mut hexes = Vec::new();

    for i in 0..5u32 {
        let pos = hex_from_offset(i as i32, 0);
        let bundle = CombatantBundle::new(
            EcsTeam::Player,
            minimal_stats(),
            2,            // armor
            0,            // magic_resist
            3 + i as i32, // speed varies so we can distinguish units
            vec![],
            minimal_equipment(),
        );
        let e = spawn_unit(world, &mut positions, pos, bundle);
        entities.push(e);
        hexes.push(pos);
    }

    for i in 0..5u32 {
        let pos = hex_from_offset(i as i32, 5);
        let bundle = CombatantBundle::new(
            EcsTeam::Enemy,
            minimal_stats(),
            1, // different armor
            0, // magic_resist
            2 + i as i32,
            vec![],
            minimal_equipment(),
        );
        let e = spawn_unit(world, &mut positions, pos, bundle);
        entities.push(e);
        hexes.push(pos);
    }

    world.insert_resource(positions);
    world.insert_resource(HexCorpses::default());
    world.insert_resource(ActiveContent(
        storyforge::content::content_view::ActiveContentData::load_global_for_tests(),
    ));
    (entities, hexes)
}

#[test]
fn from_ecs_round_trip_10_units() {
    let mut world = World::new();
    let (entities, hexes) = build_10_unit_world(&mut world);
    let mut id_map = UnitIdMap::default();
    let combat_state = run_from_ecs(&mut world, 1, &mut id_map);

    // All 10 units present.
    assert_eq!(
        combat_state.units().len(),
        10,
        "expected 10 units in CombatState"
    );

    // Every spawned entity is in the map and the engine state.
    for (entity, expected_hex) in entities.iter().zip(hexes.iter()) {
        let uid = id_map
            .get_id(*entity)
            .unwrap_or_else(|| panic!("entity {entity:?} not in UnitIdMap"));

        let unit = combat_state
            .unit(uid)
            .unwrap_or_else(|| panic!("UnitId {uid:?} not in CombatState"));

        // Position round-trip.
        assert_eq!(
            unit.pos, *expected_hex,
            "pos mismatch for entity {entity:?}"
        );

        // HP matches (alive units start at max_hp).
        assert_eq!(unit.hp(), 20);
        assert_eq!(unit.max_hp(), 20);

        // No statuses.
        assert!(unit.statuses.is_empty());

        // Reactions default.
        assert_eq!(unit.reactions_left, 1);

        // UnitIdMap inverse is consistent.
        let back = id_map.get_entity(uid).unwrap();
        assert_eq!(back, *entity);
    }

    // Teams are correct.
    let player_count = combat_state
        .units()
        .iter()
        .filter(|u| u.team == Team::Player)
        .count();
    let enemy_count = combat_state
        .units()
        .iter()
        .filter(|u| u.team == Team::Enemy)
        .count();
    assert_eq!(player_count, 5);
    assert_eq!(enemy_count, 5);

    // Round is propagated.
    assert_eq!(combat_state.round, 1);
}

/// Dead entities (Has<Dead>) are included as tombstones with hp=0.
#[test]
fn dead_unit_is_tombstone_with_hp_zero() {
    let mut world = World::new();
    let mut positions = HexPositions::default();
    let mut corpses = HexCorpses::default();

    // Spawn one alive + one dead unit.
    let alive = world
        .spawn(CombatantBundle::new(
            EcsTeam::Player,
            minimal_stats(),
            0,
            0,
            3,
            vec![],
            minimal_equipment(),
        ))
        .id();
    positions.insert(alive, hex_from_offset(0, 0));

    // Mark second unit as dead — lives in the corpse layer.
    // hp=0 matches the projector convention: `from_ecs` uses `Vital.hp <= 0`
    // (not `Has<Dead>`) to route an entity into HexCorpses.
    let dead = world
        .spawn((
            CombatantBundle::new(
                EcsTeam::Enemy,
                minimal_stats(),
                0,
                0,
                3,
                vec![],
                minimal_equipment(),
            ),
            Dead,
        ))
        .id();
    world.get_mut::<Vital>(dead).unwrap().hp = 0;
    corpses.insert(dead, hex_from_offset(1, 0));

    world.insert_resource(positions);
    world.insert_resource(corpses);
    world.insert_resource(ActiveContent(
        storyforge::content::content_view::ActiveContentData::load_global_for_tests(),
    ));

    let mut id_map = UnitIdMap::default();
    let combat_state = run_from_ecs(&mut world, 0, &mut id_map);

    assert_eq!(combat_state.units().len(), 2);

    let dead_id = id_map.get_id(dead).unwrap();
    let dead_unit = combat_state.unit(dead_id).unwrap();
    assert_eq!(dead_unit.hp(), 0, "dead unit should be tombstone with hp=0");
    assert!(!dead_unit.is_alive());

    let alive_id = id_map.get_id(alive).unwrap();
    let alive_unit = combat_state.unit(alive_id).unwrap();
    assert!(alive_unit.is_alive());
}
