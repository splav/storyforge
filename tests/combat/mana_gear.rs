//! Spawn-path tests: verify armor `mana` bonuses land on the `Mana` component
//! after `spawn_combatants` runs — the one place where ordering matters
//! (equipment must not be moved before `equipment_mana_bonus` is called).

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use std::collections::HashMap;

use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::content::armor::{ArmorDef, ArmorSlot, ArmorWeight};
use storyforge::content::classes::ClassDef;
use storyforge::content::content_view::ActiveContentData;
use storyforge::content::encounters::{EncounterDef, VictoryCondition};
use storyforge::content::scenarios::{PartyMemberDef, ScenarioDef, SceneDef};
use storyforge::game::components::{CombatStats, Mana};
use storyforge::game::resources::{
    CombatBlockedHexes, CombatEnvironment, CombatObjective, GameDb, HexCorpses, HexPositions,
    ScenarioState,
};
use storyforge::scenario::combat_scene::spawn_combatants;

// ── helpers ───────────────────────────────────────────────────────────────────

fn blank_stats() -> CombatStats {
    CombatStats {
        max_hp: 10,
        strength: 0,
        dexterity: 0,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    }
}

/// A minimal mage class with the given mana_max, equipped with `chest_id`.
fn mage_class(chest_id: &str, mana_max: i32) -> ClassDef {
    ClassDef {
        id: "mage".into(),
        name: "Mage".into(),
        stats: blank_stats(),
        speed: 3,
        abilities: vec![],
        main_hand: "unarmed".into(),
        off_hand: None,
        chest: chest_id.into(),
        legs: "bare_legs".into(),
        feet: "bare_feet".into(),
        rage_max: 0,
        mana_max,
        energy_max: 0,
        armor_proficiencies: vec![],
    }
}

/// Armor piece with a mana bonus.
fn mana_armor(id: &str, slot: ArmorSlot, mana: i32) -> ArmorDef {
    ArmorDef {
        id: id.into(),
        name: id.into(),
        slot,
        weight: ArmorWeight::Light,
        image: None,
        stats: storyforge::content::item_stats::ItemStats {
            mana,
            ..Default::default()
        },
    }
}

/// Zero-mana armor placeholder.
fn bare_armor(id: &str, slot: ArmorSlot) -> ArmorDef {
    mana_armor(id, slot, 0)
}

/// Build a minimal ActiveContentData with mage class + armor items.
fn content_for_mage(chest: ArmorDef, mana_max: i32) -> ActiveContentData {
    let mut armor = HashMap::new();
    let chest_id = chest.id.to_string();
    armor.insert(chest.id.clone(), chest);

    let legs = bare_armor("bare_legs", ArmorSlot::Legs);
    armor.insert(legs.id.clone(), legs);
    let feet = bare_armor("bare_feet", ArmorSlot::Feet);
    armor.insert(feet.id.clone(), feet);

    let cls = mage_class(&chest_id, mana_max);
    let mut classes = HashMap::new();
    classes.insert("mage".into(), cls);

    ActiveContentData {
        armor,
        classes,
        ..ActiveContentData::default()
    }
}

/// Build a ScenarioDef with one mage party member and one encounter (no enemies).
fn scenario_with_mage(content: ActiveContentData) -> ScenarioDef {
    let encounter = EncounterDef {
        id: "enc".into(),
        name: "enc".into(),
        enemies: vec![],
        victory: VictoryCondition::AllEnemiesDead,
        obstacles: vec![],
        environment: vec![],
        on_defeat: Default::default(),
        objectives: vec![],
        rewards: vec![],
    };
    let mut encounters = HashMap::new();
    encounters.insert("enc".into(), encounter);

    let member = PartyMemberDef {
        id: "mage".into(),
        name: "Mage".into(),
        race: String::new(),
        faction: None,
        path: None,
        class_id: "mage".into(),
        hex_pos: hexx::Hex::ZERO,
        template: None,
    };

    ScenarioDef {
        id: "test_scen".into(),
        name: "test_scen".into(),
        party: vec![member],
        scenes: vec![SceneDef::Combat {
            encounter_id: "enc".into(),
            location: None,
            on_victory_flags: vec![],
            requires_flag: None,
        }],
        content,
        encounters,
    }
}

/// Minimal app that can run `spawn_combatants`.
fn spawn_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatObjective>()
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<CombatEnvironment>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .insert_resource(AbilityTagCache::default());
    app
}

fn run_spawn(app: &mut App, scenario: ScenarioDef) {
    let scen_id = scenario.id.clone();
    let mut db = GameDb {
        scenarios: HashMap::new(),
        campaigns: HashMap::new(),
        campaign_order: vec![],
    };
    db.scenarios.insert(scen_id.clone(), scenario);

    app.world_mut().insert_resource(db);
    app.world_mut().insert_resource(ScenarioState {
        scenario_id: scen_id,
        scene_index: 0,
    });

    let empty_loadouts = HashMap::new();

    app.world_mut()
        .run_system_once(
            move |mut commands: Commands,
                  db: Res<GameDb>,
                  scenario: Res<ScenarioState>,
                  mut objective: ResMut<CombatObjective>,
                  mut blocked: ResMut<CombatBlockedHexes>,
                  mut environment: ResMut<CombatEnvironment>,
                  tag_cache: Res<AbilityTagCache>| {
                spawn_combatants(
                    &mut commands,
                    &db,
                    &scenario,
                    &mut objective,
                    &mut blocked,
                    &mut environment,
                    &tag_cache,
                    &empty_loadouts,
                );
            },
        )
        .expect("spawn_combatants failed");
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn mage_with_mana_robe_gets_bonus_added_to_mana_pool() {
    let cls_mana_max = 3;
    let robe_mana = 1;
    let chest = mana_armor("mage_robe", ArmorSlot::Chest, robe_mana);
    let content = content_for_mage(chest, cls_mana_max);
    let scenario = scenario_with_mage(content);

    let mut app = spawn_app();
    run_spawn(&mut app, scenario);

    let mana = app
        .world_mut()
        .query::<&Mana>()
        .single(app.world())
        .expect("expected exactly one Mana component");

    assert_eq!(
        mana.max,
        cls_mana_max + robe_mana,
        "Mana.max should be class mana_max + robe bonus"
    );
    assert_eq!(
        mana.current,
        cls_mana_max + robe_mana,
        "Mana.current should equal max at spawn"
    );
}

#[test]
fn non_caster_wearing_mana_robe_gets_no_mana_component() {
    // warrior class with mana_max == 0 — gear must NOT create a mana pool
    let chest = mana_armor("mage_robe", ArmorSlot::Chest, 1);
    let cls_mana_max = 0; // non-caster
    let content = content_for_mage(chest, cls_mana_max);
    let scenario = scenario_with_mage(content);

    let mut app = spawn_app();
    run_spawn(&mut app, scenario);

    let count = app.world_mut().query::<&Mana>().iter(app.world()).count();
    assert_eq!(count, 0, "non-caster must not receive a Mana component");
}
