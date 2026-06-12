//! Integration tests for the loadout overlay applied in `spawn_combatants`.
//!
//! Verifies that `CampaignState.loadouts` overrides the class-default `Equipment`
//! at spawn time, and that the overridden equipment flows through to the engine
//! `Unit.caster_context` (weapon_dice / armor) via `bootstrap_combat_state`.
//!
//! All tests use real content loaded from `assets/data/` so the assertions are
//! grounded in actual weapon / armor definitions.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use std::collections::HashMap;

use combat_engine::{ArmorId, WeaponId};
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::bridge::{
    apply_bridge_queues_pre_projection, bootstrap_combat_state, BridgeQueues, CombatStateRes,
    UnitIdMap,
};
use storyforge::combat::DiceRngRes;
use storyforge::content::campaigns::load_campaigns;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::item_ref::EquipmentSave;
use storyforge::content::scenarios::SceneDef;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{Combatant, Equipment, StartingHexPos};
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective, GameDb, HexCorpses,
    HexPositions, PresetInitiative, ScenarioState, TurnQueue, UiDirty,
};
use storyforge::scenario::combat_scene::spawn_combatants;

// ── App builder ───────────────────────────────────────────────────────────────

fn scenario_app(content: storyforge::content::content_view::ContentView) -> App {
    use bevy::math::Vec2;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use storyforge::combat::ai::log::{AiLogger, PendingAiLogEntries};
    use storyforge::game::messages::ActionInput;
    use storyforge::ui::animation::AnimationQueue;
    use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

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
        .init_resource::<CombatObjective>()
        .init_resource::<UiDirty>()
        .insert_resource(ActiveContent(content))
        .init_resource::<DiceRngRes>()
        .init_resource::<CombatLog>()
        .init_resource::<AnimationQueue>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials::default())
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .init_resource::<BridgeQueues>()
        .init_resource::<PresetInitiative>()
        .init_resource::<EngineTraceWriter>()
        .init_resource::<AiLogger>()
        .init_resource::<PendingAiLogEntries>()
        .add_message::<ActionInput>();
    app
}

// ── Shared systems ────────────────────────────────────────────────────────────

/// Resource injected into the app to carry the test-specific loadout map.
#[derive(Resource)]
struct TestLoadouts(HashMap<String, EquipmentSave>);

#[allow(clippy::too_many_arguments)]
fn spawn_with_loadouts(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    mut objective: ResMut<CombatObjective>,
    mut blocked: ResMut<CombatBlockedHexes>,
    mut environment: ResMut<CombatEnvironment>,
    tag_cache: Res<AbilityTagCache>,
    loadouts_res: Res<TestLoadouts>,
) {
    spawn_combatants(
        &mut commands,
        &db,
        &scenario,
        &mut objective,
        &mut blocked,
        &mut environment,
        &tag_cache,
        &loadouts_res.0,
    );
}

/// Assign `StartingHexPos` → `HexPositions` (mirrors assign_hex_positions in render.rs).
fn apply_hex_positions(
    mut commands: Commands,
    mut positions: ResMut<HexPositions>,
    q: Query<(Entity, &StartingHexPos)>,
) {
    for (e, pos) in &q {
        positions.insert(e, pos.0);
        commands.entity(e).remove::<StartingHexPos>();
    }
}

/// Read-back system: collects the main_hand WeaponId of the combatant named "Aldric".
#[derive(Resource, Default)]
struct AldricMainHand(Option<WeaponId>);

fn collect_aldric_main_hand(
    mut out: ResMut<AldricMainHand>,
    q: Query<(&Name, &Equipment), With<Combatant>>,
) {
    for (name, eq) in &q {
        if name.as_str() == "Aldric" {
            out.0 = eq.main_hand.clone();
        }
    }
}

// ── Content helpers ───────────────────────────────────────────────────────────

fn ch1_first_combat() -> (String, storyforge::content::scenarios::ScenarioDef) {
    let campaigns = load_campaigns();
    let scen = campaigns
        .scenarios
        .get("ch1")
        .expect("ch1 scenario must exist")
        .clone();
    ("ch1".to_string(), scen)
}

fn ch1_combat_scene_index(scen: &storyforge::content::scenarios::ScenarioDef) -> usize {
    scen.scenes
        .iter()
        .position(|s| matches!(s, SceneDef::Combat { .. }))
        .expect("ch1 must have a combat scene")
}

fn setup_app(
    scenario_id: &str,
    scenario: storyforge::content::scenarios::ScenarioDef,
    scene_index: usize,
    loadouts: HashMap<String, EquipmentSave>,
) -> App {
    let mut app = scenario_app(scenario.content.clone());
    let mut db = GameDb {
        scenarios: HashMap::new(),
        campaigns: HashMap::new(),
        campaign_order: Vec::new(),
    };
    db.scenarios.insert(scenario_id.to_string(), scenario);
    app.world_mut().insert_resource(db);
    app.world_mut().insert_resource(ScenarioState {
        scenario_id: scenario_id.to_string(),
        scene_index,
    });
    app.world_mut().insert_resource(TestLoadouts(loadouts));
    app.init_resource::<AldricMainHand>();
    app
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Override Aldric's weapon to `short_sword` (1d8); warrior default is `long_sword` (2d6).
/// The spawned Equipment must reflect the override.
#[test]
fn loadout_override_replaces_class_default_equipment() {
    let (scenario_id, scenario) = ch1_first_combat();
    let scene_index = ch1_combat_scene_index(&scenario);

    let mut loadouts = HashMap::new();
    loadouts.insert(
        "aldric".to_string(),
        EquipmentSave {
            main_hand: Some(WeaponId::from("short_sword")),
            off_hand: None,
            chest: ArmorId::from("plate_armor"),
            legs: ArmorId::from("plate_greaves"),
            feet: ArmorId::from("leather_boots"),
        },
    );

    let mut app = setup_app(&scenario_id, scenario, scene_index, loadouts);

    app.world_mut()
        .run_system_once(spawn_with_loadouts)
        .expect("spawn_with_loadouts failed");
    app.world_mut()
        .run_system_once(collect_aldric_main_hand)
        .expect("collect_aldric_main_hand failed");

    let aldric_weapon = &app.world().resource::<AldricMainHand>().0;
    assert_eq!(
        *aldric_weapon,
        Some(WeaponId::from("short_sword")),
        "Aldric's Equipment must have short_sword after loadout override; got: {:?}",
        aldric_weapon
    );
}

/// Unknown slug in loadouts → class default is used (no panic, no wrong equipment).
#[test]
fn unknown_slug_falls_back_to_class_default() {
    let (scenario_id, scenario) = ch1_first_combat();
    let scene_index = ch1_combat_scene_index(&scenario);

    let mut loadouts = HashMap::new();
    // "no_such_hero" won't match any party member's id.
    loadouts.insert(
        "no_such_hero".to_string(),
        EquipmentSave {
            main_hand: Some(WeaponId::from("dagger")),
            off_hand: None,
            chest: ArmorId::from("mage_robe"),
            legs: ArmorId::from("cloth_pants"),
            feet: ArmorId::from("cloth_shoes"),
        },
    );

    let mut app = setup_app(&scenario_id, scenario, scene_index, loadouts);

    app.world_mut()
        .run_system_once(spawn_with_loadouts)
        .expect("spawn_with_loadouts failed");
    app.world_mut()
        .run_system_once(collect_aldric_main_hand)
        .expect("collect_aldric_main_hand failed");

    let aldric_weapon = &app.world().resource::<AldricMainHand>().0;
    assert_eq!(
        *aldric_weapon,
        Some(WeaponId::from("long_sword")),
        "Aldric must keep long_sword (class default) when slug misses; got: {:?}",
        aldric_weapon
    );
}

/// Regression: overriding weapon flows through to engine Unit.caster_context.weapon_dice.
///
/// Replace Aldric's `long_sword` (2d6) with `short_sword` (1d8) and verify
/// that after `bootstrap_combat_state` the engine Unit's `weapon_dice.sides == 8`.
#[test]
fn override_weapon_flows_to_engine_unit_caster_context() {
    let (scenario_id, scenario) = ch1_first_combat();
    let scene_index = ch1_combat_scene_index(&scenario);

    let mut loadouts = HashMap::new();
    loadouts.insert(
        "aldric".to_string(),
        EquipmentSave {
            main_hand: Some(WeaponId::from("short_sword")),
            off_hand: None,
            chest: ArmorId::from("plate_armor"),
            legs: ArmorId::from("plate_greaves"),
            feet: ArmorId::from("leather_boots"),
        },
    );

    let mut app = setup_app(&scenario_id, scenario, scene_index, loadouts);

    app.world_mut()
        .run_system_once(spawn_with_loadouts)
        .expect("spawn_with_loadouts failed");
    app.world_mut()
        .run_system_once(apply_hex_positions)
        .expect("apply_hex_positions failed");
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .ok(); // may be no-op

    let state = app.world().resource::<CombatStateRes>().0.clone();

    // Find the warrior unit — the one whose weapon_dice.sides == 8 (short_sword 1d8).
    // long_sword (class default) is 2d6 (sides == 6).
    let warrior = state.units().iter().find(|u| {
        u.team == combat_engine::state::Team::Player
            && u.caster_context.weapon_dice.is_some_and(|d| d.sides == 8)
    });

    assert!(
        warrior.is_some(),
        "Engine Unit for Aldric must have weapon_dice.sides=8 (short_sword 1d8) after override. \
         Player units: {:?}",
        state
            .units()
            .iter()
            .filter(|u| u.team == combat_engine::state::Team::Player)
            .map(|u| (u.id, u.caster_context.weapon_dice))
            .collect::<Vec<_>>()
    );
}
