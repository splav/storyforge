//! E2E tests for T1.2 / F1+F3+F5+F11: NPC as temporary party ally via
//! `party_add` + `initial_statuses` on `UnitTemplate`.
//!
//! Tests cover:
//! - Scenario TOML parses correctly (party_add with template field).
//! - `active_party` includes/excludes Магистр after story scenes.
//! - Магистр spawns into engine `CombatState.units` (tracked by engine).
//! - Магистр gets `stunned` with `PERMANENT_DURATION` at combat start.
//! - Engine skips Магистр's turn via the existing stun path.
//! - Engine sees Магистр as same-team unit as hero.
//! - Victory when all enemies dead + Магистр alive (`KeepAlive` condition).
//! - Defeat when Магистр dies.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use storyforge::app_state::CombatPhase;
use storyforge::combat::advance_turn::check_victory_system;
use storyforge::combat::engine_bridge::{CombatStateRes, UnitIdMap};
use storyforge::combat::turn_order::build_turn_order;
use storyforge::content::content_view::ContentView;
use storyforge::content::encounters::load_encounters_from_str;
use storyforge::content::scenarios::{active_party, parse_scenario_body};
use storyforge::game::components::{
    ActionPoints, Combatant, Dead, Faction, Reactions, Speed, Team, TemplateRef, Vital,
};
use storyforge::game::resources::{CombatObjective, HexPositions};

use combat_engine::{StatusId, PERMANENT_DURATION};

#[path = "common/mod.rs"]
mod common;

use common::apps::engine::{init_engine_state, movement_app};
use common::fixtures::{base_stats, test_enemy, test_hero};

// ── fixture loading ───────────────────────────────────────────────────────────

fn campaign_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/data/campaigns/bell_under_veil")
}

fn scenario_dir() -> std::path::PathBuf {
    campaign_dir().join("ch2/scenarios/ch2_shrine")
}

/// Load and parse ch2_shrine scenario.toml (including encounters and content).
fn load_ch2_scenario() -> storyforge::content::scenarios::ScenarioDef {
    let dir = scenario_dir();
    let scen_path = dir.join("scenario.toml");
    let enc_path = dir.join("encounters.toml");
    let campaign = campaign_dir();

    let scen_src = std::fs::read_to_string(&scen_path)
        .unwrap_or_else(|e| panic!("cannot read scenario.toml: {e}"));
    let mut scen = parse_scenario_body("ch2_shrine", scen_path.to_str().unwrap(), &scen_src);

    scen.content = ContentView::load_layered(&campaign, &dir);

    let enc_src = std::fs::read_to_string(&enc_path)
        .unwrap_or_else(|e| panic!("cannot read encounters.toml: {e}"));
    let encounters = load_encounters_from_str(
        "ch2_shrine",
        enc_path.to_str().unwrap(),
        &enc_src,
        &scen.content.unit_templates,
    );
    scen.encounters = encounters.into_iter().map(|e| (e.id.clone(), e)).collect();
    scen
}

// ── spawn helpers ─────────────────────────────────────────────────────────────

fn spawn_hero(app: &mut App, hex: hexx::Hex) -> Entity {
    let e = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, hex);
    e
}

fn spawn_enemy_at(app: &mut App, hex: hexx::Hex) -> Entity {
    let e = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, hex);
    e
}

/// Spawn Магистр as a template-based party member with `TemplateRef("wounded_magister")`.
/// Mirrors what `spawn_combatants` does for template-based party members.
/// `initial_statuses` are applied engine-side by `bootstrap_combat_state` via
/// `CombatState::apply_initial_statuses` — no bridge-side StatusEffects injection.
fn spawn_magister(app: &mut App, hex: hexx::Hex) -> Entity {
    let entity = app.world_mut().spawn((
        Name::new("Магистр"),
        Combatant,
        Faction(Team::Player),
        Vital { hp: 6, max_hp: 6, armor: 0 },
        Speed(0),
        ActionPoints { action_points: 1, max_ap: 1, movement_points: 0 },
        Reactions { remaining: 1, max: 1 },
        TemplateRef("wounded_magister".to_string()),
    )).id();
    app.world_mut().resource_mut::<HexPositions>().insert(entity, hex);
    entity
}

// ── scenario parsing tests ────────────────────────────────────────────────────

/// scenario.toml parses without panic and has the expected scene count.
#[test]
fn scenario_parses_correctly() {
    let scen = load_ch2_scenario();
    assert_eq!(scen.scenes.len(), 3, "expected 3 scenes: story + combat + story");
}

/// After scene 0 (story with party_add), Магистр is in the active party.
#[test]
fn magister_joins_party_via_party_add() {
    let scen = load_ch2_scenario();
    // `active_party(scen, up_to=1)` applies effects of scenes[0].
    let party = active_party(&scen, 1);
    let names: Vec<&str> = party.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"Магистр"),
        "Магистр must be in active party after story scene; got: {names:?}"
    );
}

/// After scene 2 (story with party_remove), Магистр is no longer in the party.
#[test]
fn magister_leaves_party_via_party_remove() {
    let scen = load_ch2_scenario();
    // `active_party(scen, up_to=3)` applies all 3 scenes.
    let party = active_party(&scen, 3);
    let names: Vec<&str> = party.iter().map(|m| m.name.as_str()).collect();
    assert!(
        !names.contains(&"Магистр"),
        "Магистр must NOT be in party after post-combat scene; got: {names:?}"
    );
}

/// The template field on the party_add record is correctly parsed.
#[test]
fn party_add_has_template_field() {
    let scen = load_ch2_scenario();
    use storyforge::content::scenarios::SceneDef;
    let SceneDef::Story { party_add, .. } = &scen.scenes[0] else {
        panic!("scene 0 must be Story");
    };
    let magister = party_add.iter().find(|m| m.name == "Магистр")
        .expect("party_add must contain Магистр");
    assert_eq!(
        magister.template.as_deref(),
        Some("wounded_magister"),
        "Магистр party_add must reference template 'wounded_magister'"
    );
}

// ── engine-level tests ────────────────────────────────────────────────────────

/// Магистр (with permanent stun) spawns into `CombatState.units` — tracked by engine.
#[test]
fn magister_spawns_into_combat_state_units() {
    let mut app = movement_app();
    spawn_hero(&mut app, hexx::Hex::new(1, 1));
    spawn_magister(&mut app, hexx::Hex::new(6, 4));
    spawn_enemy_at(&mut app, hexx::Hex::new(2, 2));

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");
    init_engine_state(&mut app);

    let state = app.world().resource::<CombatStateRes>();
    assert_eq!(
        state.0.units().len(),
        3,
        "all 3 units (hero + magister + enemy) must be in engine state"
    );
}

/// At combat start, Магистр has `stunned` status with `PERMANENT_DURATION` in engine.
///
/// Статусы применяются engine-side через `CombatState::apply_initial_statuses`
/// (читает `UnitTemplate.initial_statuses` через `ContentView`).
/// Для этого теста загружаем кампейн-контент, содержащий шаблон `wounded_magister`.
#[test]
fn magister_is_stunned_at_combat_start() {
    use storyforge::content::content_view::{ActiveContent, ContentView as ContentViewStruct};

    let mut app = movement_app();

    // Load campaign content that contains the wounded_magister template.
    let campaign = campaign_dir();
    let campaign_content = ContentViewStruct::load_layered(&campaign, &campaign);
    *app.world_mut().resource_mut::<ActiveContent>() = ActiveContent(campaign_content);

    spawn_hero(&mut app, hexx::Hex::new(1, 1));
    let magister_entity = spawn_magister(&mut app, hexx::Hex::new(6, 4));
    spawn_enemy_at(&mut app, hexx::Hex::new(2, 2));

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");
    init_engine_state(&mut app);

    // Engine: CombatState must have stunned with PERMANENT_DURATION applied
    // by apply_initial_statuses (engine-side, from the template).
    let id_map = app.world().resource::<UnitIdMap>();
    let magister_uid = id_map.get_id(magister_entity)
        .expect("Магистр must be in UnitIdMap");
    let state = app.world().resource::<CombatStateRes>();
    let unit = state.0.unit(magister_uid).expect("Магистр in engine state");
    let eng_stunned = unit.statuses.iter()
        .find(|s| s.id == StatusId::from("stunned"))
        .expect("engine unit must have stunned applied from initial_statuses");
    assert_eq!(
        eng_stunned.rounds_remaining, PERMANENT_DURATION,
        "engine stunned must have PERMANENT_DURATION"
    );
}

/// Engine emits `TurnSkipped(Stunned)` for Магистр and does NOT decrement PERMANENT_DURATION.
#[test]
fn magister_skips_turns() {
    use storyforge::combat_engine::{
        action::Action,
        content::{ContentView, StatusBonuses, StatusDef, UnitTemplate},
        dice::DiceRng,
        event::{Event, TurnSkipReason},
        state::{ActiveStatus as EngineStatus, CombatState, RoundPhase, Team as EngineTeam, Unit, UnitId},
        step::step,
        AbilityId, PoolKind, RegenRule, StatusId as EngineStatusId,
    };

    let hero_id = UnitId(1);
    let magister_id = UnitId(2);

    let make_unit = |id: UnitId, team: EngineTeam, col: i32, row: i32| -> Unit {
        Unit {
            id,
            team,
            pos: storyforge::game::hex::hex_from_offset(col, row),
            hp: 10,
            max_hp: 10,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            base_speed: 3,
            speed: 3,
            reactions_left: 1,
            reactions_max: 1,
            statuses: vec![],
            summoner: None,
            caster_context: Default::default(),
            aoo_dice: None,
            auras: vec![],
            enemy_phases: vec![],
            pools: storyforge::combat_engine::enum_map::enum_map! {
                PoolKind::Hp     => Some((10, 10)),
                PoolKind::Mana   => None,
                PoolKind::Rage   => None,
                PoolKind::Energy => None,
                PoolKind::Ap     => Some((1, 1)),
                PoolKind::Mp     => Some((3, 3)),
            },
            regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
                PoolKind::Hp     => RegenRule::None,
                PoolKind::Mana   => RegenRule::Increment(1),
                PoolKind::Rage   => RegenRule::None,
                PoolKind::Energy => RegenRule::Increment(1),
                PoolKind::Ap     => RegenRule::RefillToMax,
                PoolKind::Mp     => RegenRule::RefillToMax,
            },
            template_id: None,
        }
    };

    let hero = make_unit(hero_id, EngineTeam::Player, 0, 0);
    let mut magister = make_unit(magister_id, EngineTeam::Player, 3, 0);
    magister.statuses.push(EngineStatus {
        id: EngineStatusId::from("stunned"),
        rounds_remaining: PERMANENT_DURATION,
        dot_per_tick: 0,
        applier: magister_id,
    });

    let mut state = CombatState::new(vec![hero, magister], 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![hero_id, magister_id], 0);
    // Prime hero's turn.
    struct StunContent;
    static STUNNED_DEF: std::sync::LazyLock<StatusDef> =
        std::sync::LazyLock::new(|| StatusDef { skips_turn: true, ..Default::default() });
    impl ContentView for StunContent {
        fn status_bonuses(&self, _: &EngineStatusId) -> StatusBonuses { StatusBonuses::default() }
        fn status_def(&self, id: &EngineStatusId) -> Option<&StatusDef> {
            if id.0.as_str() == "stunned" { Some(&STUNNED_DEF) } else { None }
        }
        fn ability_def(&self, _: &AbilityId) -> Option<&storyforge::combat_engine::content::AbilityDef> { None }
        fn unit_template(&self, _: &str) -> Option<UnitTemplate> { None }
    }

    let mut rng = DiceRng::with_seed(42);
    let result = step(&mut state, Action::EndTurn { actor: hero_id }, &mut rng, &StunContent);
    let (events, _ctx) = result.expect("EndTurn must succeed");

    let skipped_magister = events.iter().any(|e| {
        matches!(e, Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned }
            if *actor == magister_id)
    });
    assert!(
        skipped_magister,
        "engine must emit TurnSkipped(Stunned) for Магистр; events: {events:?}"
    );

    // Permanent duration must NOT be decremented.
    let mag = state.unit(magister_id).expect("magister in state");
    let stunned = mag.statuses.iter().find(|s| s.id.0.as_str() == "stunned")
        .expect("stunned must still be present");
    assert_eq!(
        stunned.rounds_remaining, PERMANENT_DURATION,
        "PERMANENT_DURATION must not change after TurnSkipped"
    );
}

/// Engine sees Магистр on the same team as hero (Player).
#[test]
fn engine_sees_magister_as_ally_of_hero() {
    let mut app = movement_app();
    let hero_entity = spawn_hero(&mut app, hexx::Hex::new(1, 1));
    spawn_magister(&mut app, hexx::Hex::new(6, 4));
    spawn_enemy_at(&mut app, hexx::Hex::new(2, 2));

    app.world_mut()
        .run_system_once(build_turn_order)
        .expect("build_turn_order failed");
    init_engine_state(&mut app);

    let id_map = app.world().resource::<UnitIdMap>();
    let hero_uid = id_map.get_id(hero_entity).expect("hero in id_map");
    let state = app.world().resource::<CombatStateRes>();
    let hero_team = state.0.unit(hero_uid).map(|u| u.team).expect("hero in state");

    let allies: Vec<_> = state.0.units().iter()
        .filter(|u| u.team == hero_team && u.is_alive())
        .collect();
    assert_eq!(
        allies.len(), 2,
        "engine must see 2 Player-team units (hero + magister); got {}", allies.len()
    );

    // Verify Магистр is specifically among allies via id_map reverse lookup.
    let has_magister = allies.iter().any(|u| {
        id_map.get_entity(u.id)
            .and_then(|e| app.world().get::<Name>(e))
            .map_or(false, |n| n.as_str() == "Магистр")
    });
    assert!(has_magister, "engine must see Магистр as Player-team ally of hero");
}

/// Engine-level test: `apply_initial_statuses` applies template statuses engine-side.
///
/// Uses a pure stub ContentView (no Bevy app) to verify that
/// `CombatState::apply_initial_statuses` correctly applies `initial_statuses`
/// from the template — without any bridge-side ECS injection.
#[test]
fn apply_initial_statuses_engine_side() {
    use storyforge::combat_engine::{
        content::{ContentView as EngineContentView, StatusBonuses, StatusDef, UnitTemplate},
        state::{CombatState, RoundPhase, Team as EngineTeam, Unit, UnitId},
        AbilityId, PoolKind, RegenRule, StatusId as EngineStatusId,
    };

    let unit_id = UnitId(1);

    // Build a unit with template_id = "test_template" and no initial statuses.
    let unit = Unit {
        id: unit_id,
        team: EngineTeam::Player,
        pos: storyforge::game::hex::hex_from_offset(0, 0),
        hp: 10,
        max_hp: 10,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 3,
        speed: 3,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: vec![],
        enemy_phases: vec![],
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => Some((10, 10)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((1, 1)),
            PoolKind::Mp     => Some((3, 3)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        template_id: Some("test_template".to_string()),
    };

    let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);

    // Stub ContentView: test_template has initial_statuses = ["stunned"].
    struct StubWithTemplate;
    static STUNNED_DEF: std::sync::LazyLock<StatusDef> =
        std::sync::LazyLock::new(|| StatusDef { skips_turn: true, ..Default::default() });

    impl EngineContentView for StubWithTemplate {
        fn status_bonuses(&self, _: &EngineStatusId) -> StatusBonuses { StatusBonuses::default() }
        fn status_def(&self, id: &EngineStatusId) -> Option<&StatusDef> {
            if id.0.as_str() == "stunned" { Some(&STUNNED_DEF) } else { None }
        }
        fn ability_def(&self, _: &AbilityId) -> Option<&storyforge::combat_engine::content::AbilityDef> { None }
        fn unit_template(&self, id: &str) -> Option<UnitTemplate> {
            if id == "test_template" {
                Some(UnitTemplate {
                    max_hp: 10,
                    armor: 0,
                    base_speed: 3,
                    max_ap: 1,
                    mana_max: 0,
                    energy_max: 0,
                    rage_max: 0,
                    caster_context: Default::default(),
                    aoo_dice: None,
                    auras: vec![],
                    enemy_phases: vec![],
                    regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
                        PoolKind::Hp     => RegenRule::None,
                        PoolKind::Mana   => RegenRule::Increment(1),
                        PoolKind::Rage   => RegenRule::None,
                        PoolKind::Energy => RegenRule::Increment(1),
                        PoolKind::Ap     => RegenRule::RefillToMax,
                        PoolKind::Mp     => RegenRule::RefillToMax,
                    },
                    initial_statuses: vec![EngineStatusId::from("stunned")],
                })
            } else {
                None
            }
        }
    }

    state.apply_initial_statuses(&StubWithTemplate);

    let unit = state.unit(unit_id).expect("unit must be in state");
    let stunned = unit.statuses.iter()
        .find(|s| s.id == EngineStatusId::from("stunned"))
        .expect("apply_initial_statuses must add stunned status");
    assert_eq!(
        stunned.rounds_remaining, PERMANENT_DURATION,
        "initial status must have PERMANENT_DURATION"
    );
    assert_eq!(stunned.applier, unit_id, "unit is its own applier for initial statuses");

    // Idempotency: call again — stunned must not be duplicated.
    state.apply_initial_statuses(&StubWithTemplate);
    let unit = state.unit(unit_id).expect("unit must still be in state");
    let stunned_count = unit.statuses.iter()
        .filter(|s| s.id == EngineStatusId::from("stunned"))
        .count();
    assert_eq!(stunned_count, 1, "apply_initial_statuses must be idempotent — no duplicate statuses");
}

// ── victory condition tests ───────────────────────────────────────────────────

/// All enemies dead + Магистр alive → Victory (KeepAlive + AllEnemiesDead).
#[test]
fn keep_alive_magister_victory_on_kill_all() {
    let scen = load_ch2_scenario();
    let enc = scen.encounters.get("ch2_shrine")
        .expect("ch2_shrine encounter must exist");

    let mut app = movement_app();
    app.world_mut().resource_mut::<CombatObjective>().0 = enc.victory.clone();

    spawn_hero(&mut app, hexx::Hex::new(1, 1));
    spawn_magister(&mut app, hexx::Hex::new(6, 4));

    for (i, enemy_def) in enc.enemies.iter().enumerate() {
        let e = spawn_enemy_at(&mut app, enemy_def.hex_pos);
        app.world_mut().entity_mut(e).insert((Name::new(format!("Enemy{i}")), Dead));
        app.world_mut().get_mut::<Vital>(e).unwrap().hp = 0;
    }

    app.world_mut()
        .run_system_once(check_victory_system)
        .expect("check_victory_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase, CombatPhase::Victory,
        "all enemies dead + Магистр alive must yield Victory"
    );
}

/// Магистр dies → Defeat (KeepAlive condition fails).
#[test]
fn keep_alive_magister_defeat_on_npc_death() {
    let scen = load_ch2_scenario();
    let enc = scen.encounters.get("ch2_shrine")
        .expect("ch2_shrine encounter must exist");

    let mut app = movement_app();
    app.world_mut().resource_mut::<CombatObjective>().0 = enc.victory.clone();

    spawn_hero(&mut app, hexx::Hex::new(1, 1));
    let magister = spawn_magister(&mut app, hexx::Hex::new(6, 4));
    app.world_mut().entity_mut(magister).insert(Dead);
    app.world_mut().get_mut::<Vital>(magister).unwrap().hp = 0;

    for enemy_def in &enc.enemies {
        spawn_enemy_at(&mut app, enemy_def.hex_pos);
    }

    app.world_mut()
        .run_system_once(check_victory_system)
        .expect("check_victory_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase, CombatPhase::Defeat,
        "Магистр dead while enemies alive must yield Defeat"
    );
}
