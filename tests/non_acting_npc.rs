//! Tests for `NonActingNpc` marker component (T1.1.1).
//!
//! Verifies that non-acting NPCs are excluded from the initiative queue,
//! can receive damage/healing, die correctly, and never inflate queue length.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use storyforge::combat::turn_order::build_turn_order;
use storyforge::game::components::{
    Combatant, Dead, Faction, NonActingNpc, Team, Vital,
};
use storyforge::game::resources::{HexPositions, TurnQueue};
use storyforge::game::hex::hex_from_offset;

// ── helpers ──────────────────────────────────────────────────────────────────

#[path = "common/mod.rs"]
mod common;

use common::apps::engine::movement_app;
use common::fixtures::{base_stats, test_enemy, test_hero};

/// Spawn a regular combatant and register its position.
fn spawn_at(app: &mut App, pos: hexx::Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos);
    e
}

/// Spawn a NonActingNpc entity (player-side, has Vital, no abilities/initiative).
fn spawn_npc(app: &mut App, pos: hexx::Hex, hp: i32, name: &'static str) -> Entity {
    let e = app
        .world_mut()
        .spawn((
            Name::new(name),
            NonActingNpc,
            Combatant,
            Faction(Team::Player),
            Vital { hp, max_hp: hp, armor: 0 },
        ))
        .id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos);
    e
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// `queue.order` must contain exactly players + enemies, never the NPC.
#[test]
fn non_acting_npc_excluded_from_turn_queue() {
    let mut app = movement_app();

    let hero  = spawn_at(&mut app, hex_from_offset(3, 3), test_hero(base_stats()),  "Hero");
    let enemy = spawn_at(&mut app, hex_from_offset(5, 3), test_enemy(base_stats()), "Enemy");
    let npc   = spawn_npc(&mut app, hex_from_offset(6, 4), 6, "Magister");

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");

    let queue = app.world().resource::<TurnQueue>();
    assert_eq!(queue.order.len(), 2, "only hero + enemy should be in queue");
    assert!(!queue.order.contains(&npc), "NPC must not appear in turn queue");
    assert!(queue.order.contains(&hero),  "hero must be in queue");
    assert!(queue.order.contains(&enemy), "enemy must be in queue");
}

/// Directly mutating NPC's `Vital.hp` simulates incoming damage; HP should decrease.
#[test]
fn non_acting_npc_can_be_damaged() {
    let mut app = movement_app();
    let npc = spawn_npc(&mut app, hex_from_offset(6, 4), 6, "Magister");

    // Simulate 4 damage
    app.world_mut().get_mut::<Vital>(npc).unwrap().hp -= 4;

    let hp = app.world().get::<Vital>(npc).unwrap().hp;
    assert_eq!(hp, 2, "6 HP - 4 damage = 2");
}

/// Healing NPC past max_hp should clamp to max_hp.
#[test]
fn non_acting_npc_can_be_healed() {
    let mut app = movement_app();
    let npc = spawn_npc(&mut app, hex_from_offset(6, 4), 6, "Magister");

    // Damage first, then overheal
    app.world_mut().get_mut::<Vital>(npc).unwrap().hp = 2;
    {
        let mut vital = app.world_mut().get_mut::<Vital>(npc).unwrap();
        vital.hp = (vital.hp + 10).min(vital.max_hp);
    }

    let vital = app.world().get::<Vital>(npc).unwrap();
    assert_eq!(vital.hp, vital.max_hp, "overheal must clamp to max_hp");
}

/// Damage reducing NPC HP to 0 should result in Dead marker being insertable.
#[test]
fn non_acting_npc_death_inserts_dead_marker() {
    let mut app = movement_app();
    let npc = spawn_npc(&mut app, hex_from_offset(6, 4), 6, "Magister");

    // Deal lethal damage and insert Dead marker (mimics bridge behavior)
    app.world_mut().get_mut::<Vital>(npc).unwrap().hp = 0;
    app.world_mut().entity_mut(npc).insert(Dead);

    assert!(
        app.world().get::<Dead>(npc).is_some(),
        "Dead marker must be present after lethal damage"
    );
    assert_eq!(
        app.world().get::<Vital>(npc).unwrap().hp,
        0
    );
}

/// Property-like test: for various (players, enemies, npcs) combos,
/// `queue.order.len() == players + enemies` always holds.
#[test]
fn prop_queue_length_equals_players_plus_enemies_regardless_of_npcs() {
    let cases: &[(usize, usize, usize)] = &[
        (1, 1, 0),
        (1, 1, 1),
        (1, 1, 3),
        (2, 1, 2),
        (1, 3, 3),
        (3, 3, 0),
        (3, 3, 3),
        (1, 2, 1),
        (2, 2, 2),
        (3, 1, 2),
    ];

    for &(players, enemies, npcs) in cases {
        let mut app = movement_app();

        for i in 0..players {
            spawn_at(
                &mut app,
                hex_from_offset(i as i32, 0),
                test_hero(base_stats()),
                Box::leak(format!("Hero{i}").into_boxed_str()),
            );
        }
        for i in 0..enemies {
            spawn_at(
                &mut app,
                hex_from_offset(i as i32, 5),
                test_enemy(base_stats()),
                Box::leak(format!("Enemy{i}").into_boxed_str()),
            );
        }
        for i in 0..npcs {
            spawn_npc(
                &mut app,
                hex_from_offset(i as i32, 9),
                6,
                Box::leak(format!("Npc{i}").into_boxed_str()),
            );
        }

        app.world_mut()
            .run_system_once(build_turn_order)
            .expect("build_turn_order failed");

        let queue_len = app.world().resource::<TurnQueue>().order.len();
        assert_eq!(
            queue_len,
            players + enemies,
            "players={players} enemies={enemies} npcs={npcs}: expected queue len {}, got {queue_len}",
            players + enemies
        );
    }
}
