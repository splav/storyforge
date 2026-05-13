//! Smoke tests for the Bevy ↔ combat-engine bridge.
//!
//! Test 1 (`process_action_move_writes_engine_state_and_projects_to_ecs`):
//!   Verifies the full round-trip for a Move action:
//!   1. `mirror_state_from_ecs` (PreUpdate) populates `CombatStateRes` from ECS.
//!   2. `process_action_system` (Update) consumes `ActionInput::Move` and calls
//!      `step()`, mutating `CombatStateRes`.
//!   3. `project_state_to_ecs` (PostUpdate) writes the engine state back to ECS.
//!   4. Both engine `CombatStateRes` AND ECS `HexPositions` end at the target hex.
//!   5. `movement_points` is decremented by 1 (path length) in both engine and ECS.
//!
//! Test 2 (`projector_writes_engine_mutation_to_ecs`):
//!   Verifies the projector in isolation — without going through
//!   `process_action_system`.  Uses a separate app fixture that registers only
//!   `project_state_to_ecs` (no mirror system) so a direct mutation of
//!   `CombatStateRes` is not overwritten before the projector runs.
//!
//! Test 3 (`aoo_dice_flows_from_equipment_through_process_action_system`):
//!   Verifies that `EcsContentView` correctly feeds weapon dice to the engine so
//!   AoO fires when the player disengages from an adjacent armed enemy.

use bevy::prelude::*;

use storyforge::combat::engine_bridge::{
    entity_to_uid, mirror_state_from_ecs, process_action_system, project_state_to_ecs,
    CombatStateRes, UnitIdMap,
};
use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef};
use storyforge::content::content_view::{ActiveContent, ContentView};
use storyforge::content::weapons::{HandType, WeaponDef};
use storyforge::core::{AbilityId, DiceExpr, WeaponId};
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::components::{ActionPoints, CombatStats, Equipment, Reactions, Team, Vital};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{CombatContext, HexPositions};

fn test_stats() -> CombatStats {
    CombatStats {
        max_hp: 20,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

fn test_equipment() -> Equipment {
    Equipment {
        main_hand: Some("short_sword".into()),
        off_hand: None,
        chest: "mage_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

/// Full bridge app: mirror (PreUpdate) + process_action (Update) + projector (PostUpdate).
///
/// Used for end-to-end tests where an `ActionInput` drives the full cycle.
fn bridge_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<CombatContext>()
        .init_resource::<ActiveContent>()
        .add_message::<ActionInput>()
        .add_systems(PreUpdate, mirror_state_from_ecs)
        .add_systems(Update, process_action_system)
        .add_systems(PostUpdate, project_state_to_ecs);
    app
}

/// Projector-only app: no mirror system, only the projector in PostUpdate.
///
/// Used to test `project_state_to_ecs` in isolation: we seed `CombatStateRes`
/// manually (or via one `bridge_app` update), then switch to this fixture so
/// that a direct mutation of `CombatStateRes` is not overwritten by
/// `mirror_state_from_ecs` before the projector fires.
fn projector_only_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<CombatContext>()
        .add_message::<ActionInput>()
        .add_systems(PostUpdate, project_state_to_ecs);
    app
}

/// Round-trip test: Move action flows through the full bridge and lands in ECS.
///
/// Renamed from `process_action_move_updates_engine_state_not_ecs` (step 2):
/// the projector added in step 3 closes the engine→ECS write loop, so the
/// "ECS unchanged" assertion is replaced by "ECS matches engine state".
#[test]
fn process_action_move_writes_engine_state_and_projects_to_ecs() {
    let mut app = bridge_app();

    // --- Spawn actor ---
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0); // direct neighbor — costs 1 MP

    let actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,   // armor
            6,   // speed (= starting movement_points)
            vec![],
            test_equipment(),
        ))
        .id();

    // Register position in HexPositions resource.
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor, start);

    // --- First update: mirror_state_from_ecs populates CombatStateRes ---
    app.update();

    // Verify engine state was mirrored correctly.
    let actor_uid = entity_to_uid(actor);
    {
        let state = app.world().resource::<CombatStateRes>();
        let unit = state.0.unit(actor_uid).expect("actor must be in engine state after mirror");
        assert_eq!(unit.pos, start, "engine state should reflect actor start pos after mirror");
    }

    // --- Send ActionInput::Move for the actor ---
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: vec![target] });

    // --- Second update: mirror re-mirrors (ECS unchanged), process_action_system
    //     calls step(), project_state_to_ecs writes engine state back to ECS ---
    app.update();

    // Engine state: actor at target hex.
    let (engine_pos, engine_mp) = {
        let state = app.world().resource::<CombatStateRes>();
        let unit = state.0.unit(actor_uid).expect("actor must still be in engine state");
        (unit.pos, unit.movement_points)
    };
    assert_eq!(engine_pos, target, "engine state must show actor at target hex after step()");
    assert_eq!(engine_mp, 5, "engine movement_points must be 6 - 1 = 5 after one-hex move");

    // ECS: projector has written engine state back.
    let ecs_pos = app
        .world()
        .resource::<HexPositions>()
        .get(&actor)
        .expect("actor must still have an ECS position");
    assert_eq!(ecs_pos, target, "ECS HexPositions must be updated by project_state_to_ecs");

    let ecs_mp = app
        .world()
        .entity(actor)
        .get::<ActionPoints>()
        .expect("actor must have ActionPoints")
        .movement_points;
    assert_eq!(ecs_mp, 5, "ECS movement_points must match engine after projection");
}

/// AoO integration test: real `EcsContentView` feeds weapon dice to the engine.
///
/// Setup:
/// - Player at hex A; Enemy at adjacent hex B.
/// - Enemy has `Reactions { remaining: 1 }`, a melee WeaponAttack ability, and
///   an equipped weapon (1d6, str=5 → str_mod=2 → dice bonus=+2).
/// - A synthetic `ActiveContent` is injected with the ability and weapon.
/// - Player moves to a hex not adjacent to the enemy (disengagement).
///
/// Assertion: player's `Vital.hp` is less than `max_hp` after two updates,
/// proving the engine fired AoO using real dice from `EcsContentView`.
///
/// With `ExpectedValue` (the dice source used by `process_action_system`),
/// AoO damage = round(1d6 + 2) = round(5.5) = 6; armor = 0 → hp drops from 20 to 14.
#[test]
fn aoo_dice_flows_from_equipment_through_process_action_system() {
    // Hexes: player starts at (0,0), enemy at a neighbor, escape to a non-adjacent hex.
    let player_start = hex_from_offset(0, 0);
    let enemy_pos = player_start.all_neighbors()[0]; // adjacent to player
    // Escape hex: 2 steps from enemy — must not be adjacent to enemy.
    // Take a neighbor of player_start that is NOT adjacent to enemy_pos.
    let escape_hex = player_start
        .all_neighbors()
        .into_iter()
        .find(|&h| h.unsigned_distance_to(enemy_pos) > 1)
        .expect("at least one non-adjacent neighbor must exist");

    // Ability id used as the enemy's melee ability.
    let melee_ability_id = AbilityId::from("test_attack");
    let sword_id = WeaponId::from("test_sword");

    // Synthetic content: one melee WeaponAttack ability + one weapon.
    let sword = WeaponDef {
        id: sword_id.clone(),
        name: "Test Sword".into(),
        hand: HandType::MainHand,
        dice: DiceExpr::new(1, 6, 0), // 1d6
        spell_power: 0,
        armor: 0,
        max_hp: 0,
        strength: 0,
        dexterity: 0,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    };
    let melee_ability = AbilityDef {
        id: melee_ability_id.clone(),
        name: "Test Attack".into(),
        target_type: storyforge::content::abilities::TargetType::SingleEnemy,
        range: AbilityRange::MELEE,
        effect: EffectDef::WeaponAttack,
        costs: vec![],
        cost_ap: 1,
        aoe: AoEShape::None,
        friendly_fire: false,
        statuses: vec![],
        magic_domains: vec![],
        magic_method: String::new(),
        key: None,
        ai_tags_override: None,
    };
    let mut content_view = ContentView::default();
    content_view.abilities.insert(melee_ability_id.clone(), melee_ability);
    content_view.weapons.insert(sword_id.clone(), sword);

    let mut app = bridge_app();
    // Replace the default (empty) ActiveContent with our synthetic one.
    app.insert_resource(ActiveContent(content_view));

    // Spawn player — armor=0, speed=6.
    let player = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(), // str=5 → str_mod=2
            0,
            6,
            vec![],
            Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
        ))
        .id();

    // Spawn enemy — melee ability + equipped test_sword + 1 reaction.
    let enemy_stats = CombatStats {
        max_hp: 20,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let enemy = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Enemy,
            enemy_stats,
            0,
            6,
            vec![melee_ability_id],
            Equipment {
                main_hand: Some(sword_id),
                off_hand: None,
                chest: "".into(),
                legs: "".into(),
                feet: "".into(),
            },
        ))
        .id();

    // Ensure enemy starts with 1 reaction.
    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<Reactions>()
        .unwrap()
        .remaining = 1;

    // Register positions.
    app.world_mut().resource_mut::<HexPositions>().insert(player, player_start);
    app.world_mut().resource_mut::<HexPositions>().insert(enemy, enemy_pos);

    // First update: mirror ECS → engine.
    app.update();

    // Record player's starting HP.
    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;

    // Send Move: player disengages from enemy.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor: player, path: vec![escape_hex] });

    // Second update: process_action_system calls step() with real EcsContentView.
    // AoO fires; project_state_to_ecs writes hp back to ECS.
    app.update();

    let hp_after = app.world().entity(player).get::<Vital>().unwrap().hp;
    assert!(
        hp_after < max_hp,
        "player hp ({hp_after}) should be less than max_hp ({max_hp}) after AoO from armed enemy"
    );
}

/// Projector-isolation test: direct engine mutation flows to ECS without
/// going through `process_action_system`.
///
/// Strategy: use a `projector_only_app` (no mirror) so a manual write to
/// `CombatStateRes` is not wiped by `mirror_state_from_ecs` before PostUpdate.
/// We seed the resource and `UnitIdMap` by running one full `bridge_app` update,
/// then transplant those resources into the projector-only app for the assertion.
#[test]
fn projector_writes_engine_mutation_to_ecs() {
    // --- Phase A: seed engine state via the full bridge_app ---
    let start = hex_from_offset(0, 0);

    let mut seed_app = bridge_app();

    let actor = seed_app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,
            6,
            vec![],
            test_equipment(),
        ))
        .id();

    seed_app
        .world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor, start);

    // One update seeds CombatStateRes and UnitIdMap.
    seed_app.update();

    // --- Phase B: set up projector-only app with the same entity / resources ---
    let mut app = projector_only_app();

    // Spawn the same actor entity in the new world (entity id is stable across
    // App instances; we need the same Entity bits so UnitIdMap lookups work).
    let new_actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,
            6,
            vec![],
            test_equipment(),
        ))
        .id();

    // The entity id may differ between worlds; use a fresh uid derived from
    // new_actor's entity bits for the projector-only app's UnitIdMap.
    let new_actor_uid = entity_to_uid(new_actor);

    // Place actor at start in HexPositions.
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(new_actor, start);

    // Insert the UnitIdMap entry for the new entity.
    app.world_mut()
        .resource_mut::<UnitIdMap>()
        .insert(new_actor, new_actor_uid);

    // Seed CombatStateRes with one unit at start position.
    {
        use storyforge::combat_engine::state::{CombatState, RoundPhase, Team as EngineTeam, Unit};
        let unit = Unit {
            id: new_actor_uid,
            team: EngineTeam::Player,
            pos: start,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            base_speed: 6,
            speed: 6,
            action_points: 2,
            movement_points: 6,
            reactions_left: 1,
            statuses: vec![],
            rage: None,
            mana: None,
            energy: None,
        };
        let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        app.world_mut().resource_mut::<CombatStateRes>().0 = state;
    }

    // --- Phase C: directly mutate engine state (no mirror to undo it) ---
    let target_pos = hex_from_offset(3, 2);
    let target_hp = 12_i32;
    let target_mp = 3_i32;
    let target_reactions: i32 = 0;

    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = res.0.unit_mut(new_actor_uid).expect("unit must be in engine state");
        unit.pos = target_pos;
        unit.hp = target_hp;
        unit.movement_points = target_mp;
        unit.reactions_left = target_reactions;
    }

    // --- Phase D: run projector ---
    app.update();

    // Assert all four fields were projected to ECS.
    let ecs_pos = app
        .world()
        .resource::<HexPositions>()
        .get(&new_actor)
        .expect("actor must have an ECS position");
    assert_eq!(ecs_pos, target_pos, "HexPositions must match engine pos after projection");

    let entity_ref = app.world().entity(new_actor);

    let ecs_hp = entity_ref.get::<Vital>().expect("actor must have Vital").hp;
    assert_eq!(ecs_hp, target_hp, "Vital.hp must match engine hp after projection");

    let ecs_mp = entity_ref
        .get::<ActionPoints>()
        .expect("actor must have ActionPoints")
        .movement_points;
    assert_eq!(ecs_mp, target_mp, "ActionPoints.movement_points must match engine after projection");

    let ecs_reactions = entity_ref
        .get::<Reactions>()
        .expect("actor must have Reactions")
        .remaining;
    assert_eq!(
        ecs_reactions,
        target_reactions as u8,
        "Reactions.remaining must match engine reactions_left after projection"
    );
}
