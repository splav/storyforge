//! E2E integration tests for T1.1.5: `NonActingNpc` spawn + double-victory scenarios.
//!
//! Tests load the real TOML fixture from `assets/data/…/ch2_shrine/encounters.toml`
//! to verify that content parsing, entity spawn, and combat-end logic are all
//! consistent with each other.
//!
//! Test inventory:
//! - `e2e_kill_all_with_alive_npc_is_victory`   — all enemies dead, NPC alive → Victory
//! - `e2e_kill_npc_mid_combat_is_defeat`         — NPC dead (Dead marker) → Defeat
//! - `e2e_npc_in_turn_queue_is_skipped`          — NPC entity absent from TurnQueue.order

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use storyforge::app_state::CombatPhase;
use storyforge::combat::advance_turn::check_victory_system;
use storyforge::combat::turn_order::build_turn_order;
use storyforge::content::content_view::ContentView;
use storyforge::content::encounters::load_encounters_from_str;
use storyforge::game::components::{
    Combatant, Dead, Faction, NonActingNpc, Team, Vital,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::{CombatObjective, HexPositions, TurnQueue};

#[path = "common/mod.rs"]
mod common;

use common::apps::engine::movement_app;
use common::fixtures::{base_stats, test_enemy, test_hero};

// ── fixture loading ───────────────────────────────────────────────────────────

/// Loads the ch2_shrine encounter fixture using the real campaign template map.
///
/// Uses `ContentView::load_layered` with the ch2 campaign dir to mirror the
/// production loading path in `campaigns.rs` — templates from the campaign
/// layer (`bell_under_veil/unit_templates.toml`) are resolved correctly.
fn load_ch2_shrine() -> Vec<storyforge::content::encounters::EncounterDef> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let campaign_dir = manifest.join("assets/data/campaigns/bell_under_veil");
    let scenario_dir = campaign_dir.join("ch2/scenarios/ch2_shrine");
    let enc_path = scenario_dir.join("encounters.toml");

    // Load templates the same way campaigns.rs does: global → campaign → scenario.
    let content = ContentView::load_layered(&campaign_dir, &scenario_dir);

    let src = std::fs::read_to_string(&enc_path)
        .unwrap_or_else(|e| panic!("cannot read ch2_shrine/encounters.toml: {e}"));
    load_encounters_from_str(
        "ch2_shrine",
        enc_path.to_str().unwrap(),
        &src,
        &content.unit_templates,
    )
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Spawn an acting hero and register its hex position.
fn spawn_hero(app: &mut App, hex: hexx::Hex) -> Entity {
    let e = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, hex);
    e
}

/// Spawn an acting enemy and register its hex position.
fn spawn_enemy(app: &mut App, hex: hexx::Hex) -> Entity {
    let e = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, hex);
    e
}

/// Spawn a NonActingNpc entity using data from `NpcDef` in the fixture.
fn spawn_npc_from_def(
    app: &mut App,
    npc: &storyforge::content::encounters::NpcDef,
) -> Entity {
    let hex = hex_from_offset(npc.hex_col, npc.hex_row);
    let e = app
        .world_mut()
        .spawn((
            Name::new(npc.name.clone()),
            Combatant,
            Faction(Team::Player),
            NonActingNpc,
            Vital { hp: npc.hp_current, max_hp: npc.hp_max, armor: 0 },
        ))
        .id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, hex);
    e
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// All enemies dead + NPC alive → `CombatPhase::Victory`.
///
/// Validates the full flow: fixture parses correctly, `AllOf(AllEnemiesDead,
/// KeepAlive)` condition triggers victory when enemies have `Dead` and NPC is alive.
#[test]
fn e2e_kill_all_with_alive_npc_is_victory() {
    let encounters = load_ch2_shrine();
    let enc = encounters
        .iter()
        .find(|e| e.id == "ch2_shrine")
        .expect("ch2_shrine encounter not found in fixture");

    let mut app = movement_app();

    // Set objective from real fixture.
    app.world_mut().resource_mut::<CombatObjective>().0 = enc.victory.clone();

    let hero = spawn_hero(&mut app, hex_from_offset(1, 1));
    let npc = spawn_npc_from_def(&mut app, &enc.npcs[0]);

    // Spawn enemies from fixture and immediately kill them.
    for (i, enemy_def) in enc.enemies.iter().enumerate() {
        let e = spawn_enemy(&mut app, enemy_def.hex_pos);
        // Override name to match any potential VictoryTarget lookup.
        app.world_mut()
            .entity_mut(e)
            .insert(Name::new(format!("Enemy{i}")));
        // Kill enemy: set HP to 0 and insert Dead marker.
        app.world_mut().entity_mut(e).insert(Dead);
        app.world_mut().get_mut::<Vital>(e).unwrap().hp = 0;
    }

    // Run victory check — Added<Dead> fires because Dead was just inserted.
    app.world_mut()
        .run_system_once(check_victory_system)
        .expect("check_victory_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::Victory,
        "all enemies dead + NPC alive must yield Victory (hero={:?}, npc={:?})",
        hero,
        npc,
    );
}

/// NPC dead (Dead marker inserted) while enemies are still alive → `CombatPhase::Defeat`.
#[test]
fn e2e_kill_npc_mid_combat_is_defeat() {
    let encounters = load_ch2_shrine();
    let enc = encounters
        .iter()
        .find(|e| e.id == "ch2_shrine")
        .expect("ch2_shrine encounter not found in fixture");

    let mut app = movement_app();
    app.world_mut().resource_mut::<CombatObjective>().0 = enc.victory.clone();

    spawn_hero(&mut app, hex_from_offset(1, 1));
    // Spawn enemies alive.
    for enemy_def in &enc.enemies {
        spawn_enemy(&mut app, enemy_def.hex_pos);
    }
    // Spawn NPC and then kill it.
    let npc = spawn_npc_from_def(&mut app, &enc.npcs[0]);
    app.world_mut().entity_mut(npc).insert(Dead);
    app.world_mut().get_mut::<Vital>(npc).unwrap().hp = 0;

    app.world_mut()
        .run_system_once(check_victory_system)
        .expect("check_victory_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::Defeat,
        "NPC dead while enemies alive must yield Defeat",
    );
}

/// NPC entity is absent from `TurnQueue.order` after `build_turn_order` runs.
///
/// Also validates that the fixture's NpcDef parses correctly and can be used
/// to spawn an entity that is correctly filtered by `build_turn_order`.
#[test]
fn e2e_npc_in_turn_queue_is_skipped() {
    let encounters = load_ch2_shrine();
    let enc = encounters
        .iter()
        .find(|e| e.id == "ch2_shrine")
        .expect("ch2_shrine encounter not found in fixture");

    // Fixture must have exactly 1 NPC and 2 enemies.
    assert_eq!(enc.npcs.len(), 1, "fixture must have exactly 1 NPC");
    assert_eq!(enc.enemies.len(), 2, "fixture must have exactly 2 enemies");

    let mut app = movement_app();

    let hero = spawn_hero(&mut app, hex_from_offset(1, 1));
    let npc = spawn_npc_from_def(&mut app, &enc.npcs[0]);
    // Spawn enemies (acting combatants, not NPCs).
    let mut enemy_entities = Vec::new();
    for enemy_def in &enc.enemies {
        let e = spawn_enemy(&mut app, enemy_def.hex_pos);
        enemy_entities.push(e);
    }

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");

    let queue = app.world().resource::<TurnQueue>();
    assert!(
        !queue.order.contains(&npc),
        "NonActingNpc must not appear in TurnQueue.order"
    );
    assert!(queue.order.contains(&hero), "hero must be in queue");
    for enemy in &enemy_entities {
        assert!(queue.order.contains(enemy), "enemy {enemy:?} must be in queue");
    }
    assert_eq!(
        queue.order.len(),
        1 + enc.enemies.len(),
        "queue length must equal hero + enemies (NPC excluded)"
    );
}
