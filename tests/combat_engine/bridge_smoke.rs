//! Smoke tests for the Bevy ↔ combat-engine bridge.
//!
//! Test 1 (`process_action_move_writes_engine_state_and_projects_to_ecs`):
//!   Verifies the full round-trip for a Move action:
//!   1. `init_state_from_ecs` populates `CombatStateRes` from ECS (called via helper).
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
    apply_phase_transitions_system, entity_to_uid, init_state_from_ecs, process_action_system,
    project_state_to_ecs, CombatStateRes, PendingPhaseTransitions, UnitIdMap,
};
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef};
use storyforge::content::content_view::{ActiveContent, ContentView};
use storyforge::content::statuses::StatusDef;
use storyforge::content::weapons::{HandType, WeaponDef};
use storyforge::combat::DiceRngRes;
use storyforge::core::{AbilityId, DiceExpr, StatusId, WeaponId};
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    ActionPoints, ActiveStatus, BonusMovement, CombatStats, Equipment, Reactions, StatusEffects,
    Team, UnitToken, Vital,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{CombatContext, HexPositions, TurnQueue};
use storyforge::ui::animation::{AnimationQueue, PendingAnim};
use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

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

/// Full bridge app: process_action + projector chained (Update).
///
/// Used for end-to-end tests where an `ActionInput` drives the full cycle.
/// Engine state is seeded explicitly via `init_bridge_engine_state` after spawning
/// units (bridge_app has no state machine, so OnEnter cannot be used).
fn bridge_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatContext>()
        .init_resource::<ActiveContent>()
        .init_resource::<DiceRngRes>()
        .init_resource::<CombatLog>()
        .init_resource::<AnimationQueue>()
        // HexGridOffset has no Default — insert a zero offset (no screen offset needed in tests).
        .insert_resource(HexGridOffset(Vec2::ZERO))
        // Stub visual resources: process_action_system requires these for spawn.
        // Default handles are zero-cost in tests (no renderer runs).
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials {
            empty: Handle::default(),
            player: Handle::default(),
            enemy: Handle::default(),
            dead: Handle::default(),
            in_range: Handle::default(),
            in_range_dim: Handle::default(),
            move_range: Handle::default(),
            border_active: Handle::default(),
            border_target: Handle::default(),
            border_in_range: Handle::default(),
            border_in_range_dim: Handle::default(),
            border_move: Handle::default(),
            aoe_preview: Handle::default(),
            border_aoe: Handle::default(),
            token_player: Handle::default(),
            token_enemy: Handle::default(),
            token_dead: Handle::default(),
        })
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .init_resource::<PendingPhaseTransitions>()
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (process_action_system, project_state_to_ecs, apply_phase_transitions_system).chain(),
        );
    app
}

/// Seed `CombatStateRes` from ECS after spawning units in bridge tests.
///
/// bridge_app has no Bevy state machine, so `OnEnter(AwaitCommand)` cannot fire.
/// Call this once after all units are spawned and positions registered, before
/// the first `app.update()` that runs `process_action_system`.
fn init_bridge_engine_state(app: &mut App) {
    use bevy::ecs::system::RunSystemOnce;
    app.world_mut()
        .run_system_once(init_state_from_ecs)
        .expect("init_state_from_ecs failed");
}

/// Projector-only app: only the projector in PostUpdate.
///
/// Used to test `project_state_to_ecs` in isolation: we seed `CombatStateRes`
/// manually (or via one `bridge_app` update), then switch to this fixture so
/// that a direct mutation of `CombatStateRes` is not overwritten by an init
/// pass before the projector fires.
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

    // Seed engine state from ECS before first update.
    init_bridge_engine_state(&mut app);

    // Verify engine state was initialized correctly.
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

    // --- Update: process_action_system calls step(), project_state_to_ecs writes engine state back to ECS ---
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

    // Seed engine state from ECS.
    init_bridge_engine_state(&mut app);

    // Record player's starting HP.
    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;

    // Send Move: player disengages from enemy.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor: player, path: vec![escape_hex] });

    // Update: process_action_system calls step() with real EcsContentView.
    // AoO fires; project_state_to_ecs writes hp back to ECS.
    app.update();

    let hp_after = app.world().entity(player).get::<Vital>().unwrap().hp;
    assert!(
        hp_after < max_hp,
        "player hp ({hp_after}) should be less than max_hp ({max_hp}) after AoO from armed enemy"
    );
}

/// Stunned-filter test: a stunned (skips_turn=true) adjacent enemy must be
/// excluded from `aoo_per_unit` so the engine fires zero AoOs on disengage.
///
/// Pins the filter block at engine_bridge.rs lines 279-291: if the filter is
/// removed, the enemy would fire AoO and the player's HP would drop.
#[test]
fn aoo_does_not_fire_from_stunned_enemy() {
    let player_start = hex_from_offset(0, 0);
    let enemy_pos = player_start.all_neighbors()[0];
    let escape_hex = player_start
        .all_neighbors()
        .into_iter()
        .find(|&h| h.unsigned_distance_to(enemy_pos) > 1)
        .expect("at least one non-adjacent neighbor must exist");

    let melee_ability_id = AbilityId::from("test_attack_stunned");
    let sword_id = WeaponId::from("test_sword_stunned");
    let stun_id = StatusId::from("test_stun");

    let sword = WeaponDef {
        id: sword_id.clone(),
        name: "Test Sword".into(),
        hand: HandType::MainHand,
        dice: DiceExpr::new(1, 6, 0),
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
    // Stub StatusDef with skips_turn = true.
    let stun_def = StatusDef {
        id: stun_id.clone(),
        name: "Test Stun".into(),
        armor_bonus: 0,
        damage_taken_bonus: 0,
        skips_turn: true,
        forces_targeting: false,
        dot_dice: None,
        blocks_mana_abilities: false,
        speed_bonus: 0,
        hp_percent_dot: 0,
        ai_controlled: false,
        causes_disadvantage: false,
        buff_class: None,
    };

    let mut content_view = ContentView::default();
    content_view.abilities.insert(melee_ability_id.clone(), melee_ability);
    content_view.weapons.insert(sword_id.clone(), sword);
    content_view.statuses.insert(stun_id.clone(), stun_def);

    let mut app = bridge_app();
    app.insert_resource(ActiveContent(content_view));

    // Spawn player — armor=0, speed=6.
    let player = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,
            6,
            vec![],
            Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
        ))
        .id();

    // Spawn enemy with melee ability + sword + 1 reaction.
    let enemy = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Enemy,
            test_stats(),
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

    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<Reactions>()
        .unwrap()
        .remaining = 1;

    // Apply stun status to the enemy.
    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(ActiveStatus {
            id: stun_id,
            rounds_remaining: 1,
            applier: player,
            dot_per_tick: 0,
        });

    app.world_mut().resource_mut::<HexPositions>().insert(player, player_start);
    app.world_mut().resource_mut::<HexPositions>().insert(enemy, enemy_pos);

    // Seed engine state from ECS.
    init_bridge_engine_state(&mut app);

    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;
    let enemy_reactions_before = app.world().entity(enemy).get::<Reactions>().unwrap().remaining;

    // Player disengages from stunned enemy.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor: player, path: vec![escape_hex] });

    app.update();

    let hp_after = app.world().entity(player).get::<Vital>().unwrap().hp;
    let enemy_reactions_after = app.world().entity(enemy).get::<Reactions>().unwrap().remaining;

    assert_eq!(
        hp_after, max_hp,
        "player hp must be unchanged — stunned enemy must not fire AoO"
    );
    assert_eq!(
        enemy_reactions_after, enemy_reactions_before,
        "stunned enemy reactions must be unchanged — no AoO was fired"
    );
}

// ── B1: new side-effect tests ────────────────────────────────────────────────

/// Helper to build synthetic melee content for AoO tests.
fn make_melee_content(ability_id: &AbilityId, weapon_id: &WeaponId) -> ContentView {
    let sword = WeaponDef {
        id: weapon_id.clone(),
        name: "Test Sword".into(),
        hand: HandType::MainHand,
        dice: DiceExpr::new(1, 6, 0),
        spell_power: 0, armor: 0, max_hp: 0,
        strength: 0, dexterity: 0, constitution: 0,
        intelligence: 0, wisdom: 0, charisma: 0,
    };
    let melee_ability = AbilityDef {
        id: ability_id.clone(),
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
    let mut cv = ContentView::default();
    cv.abilities.insert(ability_id.clone(), melee_ability);
    cv.weapons.insert(weapon_id.clone(), sword);
    cv
}

/// The bridge emits `CombatEvent::OpportunityAttack` when the engine fires an AoO.
#[test]
fn engine_emits_combat_log_opportunity_attack() {
    let player_start = hex_from_offset(0, 0);
    let enemy_pos = player_start.all_neighbors()[0];
    let escape_hex = player_start
        .all_neighbors()
        .into_iter()
        .find(|&h| h.unsigned_distance_to(enemy_pos) > 1)
        .unwrap();

    let ability_id = AbilityId::from("b1_aoo_attack");
    let weapon_id = WeaponId::from("b1_aoo_sword");
    let content = make_melee_content(&ability_id, &weapon_id);

    let mut app = bridge_app();
    app.insert_resource(ActiveContent(content));

    let player = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let enemy = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![ability_id],
        Equipment { main_hand: Some(weapon_id), off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    app.world_mut().entity_mut(enemy).get_mut::<Reactions>().unwrap().remaining = 1;
    app.world_mut().resource_mut::<HexPositions>().insert(player, player_start);
    app.world_mut().resource_mut::<HexPositions>().insert(enemy, enemy_pos);

    init_bridge_engine_state(&mut app);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor: player, path: vec![escape_hex] });
    app.update();

    let log = app.world().resource::<CombatLog>();
    let aoo_events: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::OpportunityAttack { attacker, target, damage, killed } = e {
            Some((*attacker, *target, *damage, *killed))
        } else { None }
    }).collect();

    assert_eq!(aoo_events.len(), 1, "exactly one OpportunityAttack expected, got {:?}", aoo_events);
    let (att, tgt, dmg, killed) = aoo_events[0];
    assert_eq!(att, enemy, "attacker must be the enemy");
    assert_eq!(tgt, player, "target must be the player");
    assert!(dmg > 0, "damage must be positive");
    assert!(!killed, "player should not die from one AoO with default HP");
}

/// The bridge emits a single aggregated `CombatEvent::UnitMoved`.
#[test]
fn engine_emits_combat_log_unit_moved() {
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0);

    let mut app = bridge_app();
    let actor = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![], test_equipment(),
    )).id();
    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);
    init_bridge_engine_state(&mut app);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: vec![target] });
    app.update();

    let log = app.world().resource::<CombatLog>();
    let moved_events: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::UnitMoved { actor: a, from, to } = e {
            Some((*a, *from, *to))
        } else { None }
    }).collect();

    assert_eq!(moved_events.len(), 1, "exactly one UnitMoved expected");
    let (a, from, to) = moved_events[0];
    assert_eq!(a, actor);
    assert_eq!(from, start);
    assert_eq!(to, target);
}

/// The bridge enqueues a `PendingAnim::Movement` with correct waypoints.
#[test]
fn engine_enqueues_movement_animation() {
    let start = hex_from_offset(0, 0);
    let step1 = hex_from_offset(1, 0);

    let mut app = bridge_app();
    let actor = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![], test_equipment(),
    )).id();
    // Spawn a token entity pointing at the actor.
    let token_entity = app.world_mut().spawn(UnitToken(actor)).id();
    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);
    init_bridge_engine_state(&mut app);

    let path = vec![step1];
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: path.clone() });
    app.update();

    let queue = app.world().resource::<AnimationQueue>();
    assert_eq!(queue.0.len(), 1, "one animation should be enqueued");
    match &queue.0[0] {
        PendingAnim::Movement { token, waypoints } => {
            assert_eq!(*token, token_entity, "token entity must match");
            // waypoints = [start, step1] → path.len() + 1 = 2
            assert_eq!(
                waypoints.len(),
                path.len() + 1,
                "waypoints should be start + each path step"
            );
        }
        other => panic!("expected Movement animation, got {:?}", std::mem::discriminant(other)),
    }
}

/// The projector removes `BonusMovement` when `movement_points` reaches zero.
#[test]
fn projector_removes_bonus_movement_when_mp_zero() {
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0);

    let mut app = bridge_app();

    // Spawn actor with movement_points = 1 (just enough for a 1-step path).
    let actor = app.world_mut().spawn((
        CombatantBundle::new(Team::Player, test_stats(), 0, 1, vec![], test_equipment()),
        BonusMovement,
    )).id();
    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);
    init_bridge_engine_state(&mut app);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: vec![target] });
    app.update();

    assert!(
        app.world().entity(actor).get::<BonusMovement>().is_none(),
        "BonusMovement must be removed when movement_points reaches zero"
    );
}

/// The bridge inserts `Dead` and emits `CombatEvent::UnitDied` when the mover
/// is killed by an AoO mid-path.
#[test]
fn engine_inserts_dead_marker_on_aoo_kill() {
    let player_start = hex_from_offset(0, 0);
    let enemy_pos = player_start.all_neighbors()[0];
    let escape_hex = player_start
        .all_neighbors()
        .into_iter()
        .find(|&h| h.unsigned_distance_to(enemy_pos) > 1)
        .unwrap();

    let ability_id = AbilityId::from("b1_kill_attack");
    let weapon_id = WeaponId::from("b1_kill_sword");
    let content = make_melee_content(&ability_id, &weapon_id);

    let mut app = bridge_app();
    app.insert_resource(ActiveContent(content));

    // Player with hp=1 — any hit is lethal.
    let weak_stats = CombatStats {
        max_hp: 1,
        strength: 5, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let player = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, weak_stats, 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let enemy = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![ability_id],
        Equipment { main_hand: Some(weapon_id), off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    app.world_mut().entity_mut(enemy).get_mut::<Reactions>().unwrap().remaining = 1;
    app.world_mut().resource_mut::<HexPositions>().insert(player, player_start);
    app.world_mut().resource_mut::<HexPositions>().insert(enemy, enemy_pos);

    // Script the dice: roll maximum damage (6) to guarantee a kill on hp=1.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[6]);

    init_bridge_engine_state(&mut app);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor: player, path: vec![escape_hex] });
    app.update();

    // Dead component must be inserted.
    assert!(
        app.world().entity(player).get::<storyforge::game::components::Dead>().is_some(),
        "player must have Dead component after lethal AoO"
    );

    // CombatLog must contain UnitDied.
    let log = app.world().resource::<CombatLog>();
    let died = log.0.iter().any(|e| matches!(e, CombatEvent::UnitDied { entity } if *entity == player));
    assert!(died, "CombatLog must contain UnitDied for the player");
}

/// Projector-isolation test: direct engine mutation flows to ECS without
/// going through `process_action_system`.
///
/// Strategy: use a `projector_only_app` (no mirror) so a manual write to
/// `CombatStateRes` is not wiped before PostUpdate.
/// We seed the resource and `UnitIdMap` via `init_bridge_engine_state`,
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

    // Seed engine state from ECS (no mirror system in bridge_app).
    init_bridge_engine_state(&mut seed_app);

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
            max_ap: 2,
            movement_points: 6,
            reactions_left: 1,
            reactions_max: 1,
            statuses: vec![],
            rage: None,
            mana: None,
            energy: None,
            summoner: None,
            caster_context: Default::default(),
            aoo_dice: None,
            auras: Vec::new(),
            enemy_phases: Vec::new(),
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

// ── Phase 2 step 7b: Cast event → CombatLog translation tests ───────────────

/// Cast with `EffectDef::Damage` emits `AbilityUsed` + `DamageResult` in CombatLog.
///
/// Caster has strength=0 (str_mod=0) so damage = dice bonus only (5).
/// Target has armor=0, so armor_reduced=0 and final_damage=5.
/// The crit-fail d20 is scripted to 11 (non-1) to ensure normal resolution.
#[test]
fn cast_emits_damage_result_log_entry() {
    use storyforge::content::abilities::TargetType;

    let caster_pos = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("dmg_ability");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Fireball".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        // 0d1+5 = constant 5; no random draws beyond the crit-fail d20.
        effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) },
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

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    // Caster: strength=0 so str_mod=0; armor=0.
    let zero_str_stats = CombatStats { max_hp: 20, strength: 0, dexterity: 5, constitution: 10, intelligence: 0, wisdom: 10, charisma: 10 };
    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, zero_str_stats, 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let target = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_hex);

    init_bridge_engine_state(&mut app);

    // Give caster 2 AP so post-cast AP=1 is visible; no mana needed.
    let caster_uid = entity_to_uid(caster);
    app.world_mut().resource_mut::<CombatStateRes>().0.unit_mut(caster_uid).unwrap().action_points = 2;

    // Script: first roll = crit-fail d20 (must be non-1); no other random draws.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: caster, ability: ability_id, target, target_pos: target_hex });

    app.update();

    let log = app.world().resource::<CombatLog>();

    // Exactly one AbilityUsed with is_aoe=false.
    let ability_used: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::AbilityUsed { actor: a, ability_name, target: t, is_aoe, .. } = e {
            Some((*a, ability_name.clone(), *t, *is_aoe))
        } else { None }
    }).collect();
    assert_eq!(ability_used.len(), 1, "expected exactly one AbilityUsed, got {:?}", ability_used);
    let (au_actor, au_name, au_target, au_aoe) = &ability_used[0];
    assert_eq!(*au_actor, caster);
    assert_eq!(au_name, "Fireball");
    assert_eq!(*au_target, target);
    assert!(!au_aoe, "ability is not AoE");

    // Exactly one DamageResult with final_damage=5.
    let dmg_results: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::DamageResult { target: t, final_damage, armor_reduced, .. } = e {
            Some((*t, *final_damage, *armor_reduced))
        } else { None }
    }).collect();
    assert_eq!(dmg_results.len(), 1, "expected exactly one DamageResult, got {:?}", dmg_results);
    let (dr_target, dr_dmg, dr_armor) = dmg_results[0];
    assert_eq!(dr_target, target, "DamageResult target must be the target entity");
    assert_eq!(dr_dmg, 5, "final_damage must be 5 (0d1+5, str_mod=0, armor=0)");
    assert_eq!(dr_armor, 0, "armor_reduced must be 0");
}

/// Cast with status-only ability emits `AbilityUsed` + `StatusApplied` in CombatLog.
///
/// The ability has `EffectDef::None` and one status on the target.
#[test]
fn cast_emits_status_applied_log_entry() {
    use storyforge::content::abilities::{StatusApplication, StatusOn, TargetType};
    use storyforge::core::StatusId;

    let caster_pos = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("burning_touch");
    let status_id = StatusId::from("burning");

    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Burning Touch".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        effect: EffectDef::None,
        costs: vec![],
        cost_ap: 1,
        aoe: AoEShape::None,
        friendly_fire: false,
        statuses: vec![StatusApplication { status: status_id.clone(), duration_rounds: 2, on: StatusOn::Target }],
        magic_domains: vec![],
        magic_method: String::new(),
        key: None,
        ai_tags_override: None,
    };

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let target = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_hex);

    init_bridge_engine_state(&mut app);

    let caster_uid = entity_to_uid(caster);
    app.world_mut().resource_mut::<CombatStateRes>().0.unit_mut(caster_uid).unwrap().action_points = 2;

    // Script crit-fail d20 to 11 (non-1).
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: caster, ability: ability_id, target, target_pos: target_hex });

    app.update();

    let log = app.world().resource::<CombatLog>();

    let status_events: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::StatusApplied { target: t, status } = e {
            Some((*t, status.clone()))
        } else { None }
    }).collect();

    assert_eq!(status_events.len(), 1, "expected exactly one StatusApplied, got {:?}", status_events);
    let (ev_target, ev_status) = &status_events[0];
    assert_eq!(*ev_target, target, "StatusApplied target must be the target entity");
    assert_eq!(*ev_status, status_id, "StatusApplied status must be 'burning'");
}

/// Cast with mana cost emits `ManaChanged` in CombatLog.
///
/// Ability costs 3 mana; caster starts with mana=(10,10).
/// After cast: mana=(7,10) → bridge diff emits ManaChanged.
#[test]
fn cast_emits_mana_changed_log_entry() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use storyforge::core::ResourceKind;

    let caster_pos = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("mana_blast");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Mana Blast".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        effect: EffectDef::None,
        costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
        cost_ap: 0,   // no AP cost so we don't need to bump AP
        aoe: AoEShape::None,
        friendly_fire: false,
        statuses: vec![],
        magic_domains: vec![],
        magic_method: String::new(),
        key: None,
        ai_tags_override: None,
    };

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let target = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_hex);

    init_bridge_engine_state(&mut app);

    // Set mana pool and ensure AP is available.
    let caster_uid = entity_to_uid(caster);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(caster_uid).unwrap();
        unit.mana = Some((10, 10));
        unit.action_points = 2;
    }

    // Script crit-fail d20 to 11 (non-1).
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: caster, ability: ability_id, target, target_pos: target_hex });

    app.update();

    let log = app.world().resource::<CombatLog>();

    let mana_events: Vec<_> = log.0.iter().filter_map(|e| {
        if let CombatEvent::ManaChanged { actor: a, current, max } = e {
            Some((*a, *current, *max))
        } else { None }
    }).collect();

    assert_eq!(mana_events.len(), 1, "expected exactly one ManaChanged, got {:?}", mana_events);
    let (mc_actor, mc_current, mc_max) = mana_events[0];
    assert_eq!(mc_actor, caster, "ManaChanged actor must be the caster");
    assert_eq!(mc_current, 7, "mana after cast must be 10 - 3 = 7");
    assert_eq!(mc_max, 10, "mana max must be 10");
}

// ── Phase 2 step 7a: ActionInput::Cast routing smoke test ────────────────────

/// Verify the bridge routes `ActionInput::Cast` into `step()` and the engine
/// mutates `CombatStateRes` (cost paid).  Event translation lands in step 7b;
/// here we just pin that the routing wiring works end-to-end.
#[test]
fn process_action_system_routes_cast_into_engine() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use storyforge::core::ResourceKind;

    let mut app = bridge_app();

    // --- Spawn caster + target ---
    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let caster = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            test_stats(),
            0,    // armor
            6,
            vec!["zap".into()],
            test_equipment(),
        ))
        .id();
    let target = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Enemy,
            test_stats(),
            0,
            6,
            vec![],
            test_equipment(),
        ))
        .id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_pos);

    // Register a Cast-able ability with a mana cost in ActiveContent.
    let zap_id = AbilityId::from("zap");
    let zap_def = AbilityDef {
        id: zap_id.clone(),
        name: "zap".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        effect: EffectDef::None,  // No damage in 7a — just verify cost flows through
        costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
        cost_ap: 1,
        aoe: AoEShape::None,
        friendly_fire: false,
        statuses: Vec::new(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        key: None,
        ai_tags_override: None,
    };
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(zap_id.clone(), zap_def);

    // Seed engine state from ECS.
    init_bridge_engine_state(&mut app);

    // CombatantBundle default AP=1; bump to 2 so post-cast AP=1 is observable.
    // Mana isn't a default Bevy component on CombatantBundle — set on engine
    // state directly so PayCost has a pool to deduct from.
    let caster_uid = entity_to_uid(caster);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(caster_uid).expect("caster in engine state");
        unit.action_points = 2;
        unit.mana = Some((10, 10));
    }

    // --- Send ActionInput::Cast for the caster ---
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast {
            actor: caster,
            ability: zap_id,
            target,
            target_pos,
        });

    app.update();

    // Engine state: caster's AP and mana paid.
    let state = app.world().resource::<CombatStateRes>();
    let caster_unit = state.0.unit(caster_uid).expect("caster still in state");
    assert_eq!(caster_unit.action_points, 1, "AP cost paid");
    assert_eq!(caster_unit.mana, Some((7, 10)), "Mana cost paid (10 - 3)");
}

// ── Phase 2 step 7c: Mana + StatusEffects projection tests ──────────────────

/// Projector writes `Mana.current` from engine state.
///
/// `CombatantBundle` does not include `Mana`, so it is inserted manually.
/// Engine state is seeded with `mana = Some((4, 10))`; after `app.update()`
/// the ECS `Mana.current` must equal 4.
#[test]
fn projector_writes_mana_from_engine_state() {
    use storyforge::game::components::Mana;
    use storyforge::combat_engine::state::{CombatState, RoundPhase, Team as EngineTeam, Unit};

    let start = hex_from_offset(0, 0);
    let mut app = projector_only_app();

    let actor = app
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

    // Add Mana component — not part of CombatantBundle by default.
    app.world_mut()
        .entity_mut(actor)
        .insert(Mana { current: 10, max: 10 });

    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);

    let actor_uid = entity_to_uid(actor);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);

    // Seed engine state with mana pool at full.
    let unit = Unit {
        id: actor_uid,
        team: EngineTeam::Player,
        pos: start,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 1,
        max_ap: 1,
        movement_points: 6,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: Some((10, 10)),
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    };
    let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Mutate engine mana to simulate a cast that spent mana.
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.mana = Some((4, 10));
    }

    app.update();

    let mana = app
        .world()
        .entity(actor)
        .get::<Mana>()
        .expect("actor must have Mana");
    assert_eq!(mana.current, 4, "Mana.current must match engine after projection");
}

/// Projector writes `StatusEffects` from engine state.
///
/// Engine state carries one status `(poison, actor_uid)`; after `app.update()`
/// the ECS `StatusEffects.0` must contain exactly that entry.
#[test]
fn projector_writes_statuses_from_engine_state() {
    use storyforge::combat_engine::state::{
        ActiveStatus as EngineActiveStatus, CombatState, RoundPhase, Team as EngineTeam, Unit,
    };

    let start = hex_from_offset(0, 0);
    let mut app = projector_only_app();

    let actor = app
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

    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);

    let actor_uid = entity_to_uid(actor);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);

    // Seed engine state with no statuses yet.
    let unit = Unit {
        id: actor_uid,
        team: EngineTeam::Player,
        pos: start,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 1,
        max_ap: 1,
        movement_points: 6,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    };
    let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Push a status into engine state (simulates Cast having applied poison).
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.statuses.push(EngineActiveStatus {
            id: StatusId::from("poison"),
            rounds_remaining: 3,
            dot_per_tick: 2,
            applier: actor_uid,
        });
    }

    app.update();

    let status_effects = app
        .world()
        .entity(actor)
        .get::<StatusEffects>()
        .expect("actor must have StatusEffects");
    assert_eq!(status_effects.0.len(), 1, "exactly one status projected");
    let s = &status_effects.0[0];
    assert_eq!(s.id, StatusId::from("poison"), "status id must match");
    assert_eq!(s.rounds_remaining, 3, "rounds_remaining must match");
    assert_eq!(s.dot_per_tick, 2, "dot_per_tick must match");
    assert_eq!(s.applier, actor, "applier entity must resolve to actor");
}

/// Projector preserves aura-applied ECS statuses that the engine doesn't know about.
///
/// The aura entry (written by `apply_auras_system` in TurnStart, after
/// `init_state_from_ecs`) has a different applier entity than the
/// ability-applied entry in engine state.  After projection:
/// - "aura_buff" (aura_source applier, not in engine) → preserved.
/// - "burning"   (actor_uid applier, only in engine)  → projected in.
#[test]
fn projector_preserves_aura_applied_status_during_cast_projection() {
    use storyforge::combat_engine::state::{
        ActiveStatus as EngineActiveStatus, CombatState, RoundPhase, Team as EngineTeam, Unit,
    };

    let start = hex_from_offset(0, 0);
    let start2 = hex_from_offset(1, 0);
    let mut app = projector_only_app();

    let actor = app
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
    let aura_source = app
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

    app.world_mut().resource_mut::<HexPositions>().insert(actor, start);
    app.world_mut().resource_mut::<HexPositions>().insert(aura_source, start2);

    let actor_uid = entity_to_uid(actor);
    let aura_source_uid = entity_to_uid(aura_source);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);
    app.world_mut().resource_mut::<UnitIdMap>().insert(aura_source, aura_source_uid);

    // Pre-seed ECS: actor already has an aura-applied status (aura_buff from aura_source).
    // This simulates what apply_auras_system writes in TurnStart, which the engine
    // doesn't know about because init_state_from_ecs ran before TurnStart.
    app.world_mut()
        .entity_mut(actor)
        .get_mut::<StatusEffects>()
        .expect("actor must have StatusEffects")
        .0
        .push(ActiveStatus {
            id: StatusId::from("aura_buff"),
            rounds_remaining: 1,
            dot_per_tick: 0,
            applier: aura_source,
        });

    // Seed engine state: actor has no statuses (engine was seeded before aura was applied).
    let actor_unit = Unit {
        id: actor_uid,
        team: EngineTeam::Player,
        pos: start,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 1,
        max_ap: 1,
        movement_points: 6,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    };
    let aura_unit = Unit {
        id: aura_source_uid,
        team: EngineTeam::Player,
        pos: start2,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 1,
        max_ap: 1,
        movement_points: 6,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    };
    let state = CombatState::new(vec![actor_unit, aura_unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Engine: Cast has applied "burning" to actor (self-applied).
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.statuses.push(EngineActiveStatus {
            id: StatusId::from("burning"),
            rounds_remaining: 2,
            dot_per_tick: 1,
            applier: actor_uid,
        });
    }

    app.update();

    let status_effects = app
        .world()
        .entity(actor)
        .get::<StatusEffects>()
        .expect("actor must have StatusEffects");
    assert_eq!(
        status_effects.0.len(),
        2,
        "both aura_buff (preserved) and burning (projected) must be present"
    );

    let ids: Vec<&str> = status_effects.0.iter().map(|s| s.id.0.as_str()).collect();
    assert!(ids.contains(&"aura_buff"), "aura_buff must be preserved");
    assert!(ids.contains(&"burning"), "burning must be projected from engine");

    let aura_entry = status_effects
        .0
        .iter()
        .find(|s| s.id.0 == "aura_buff")
        .unwrap();
    assert_eq!(aura_entry.applier, aura_source, "aura_buff applier must be aura_source entity");

    let burning_entry = status_effects
        .0
        .iter()
        .find(|s| s.id.0 == "burning")
        .unwrap();
    assert_eq!(burning_entry.applier, actor, "burning applier must resolve to actor entity");
}

// ── Phase 2 step 7d: crit-fail event → CombatLog translation tests ───────────

/// Bridge translates `Event::CritFailed { outcome: Miss }` → `CombatEvent::CriticalMiss`.
///
/// Caster has mana=(10,10), ability costs 1 AP + 3 mana, EffectDef::None.
/// DiceRng scripted to 1 (crit-fail). EcsContentView defaults crit_fail_outcome
/// to Miss (see build_ecs_content_view TODO). After update: CombatLog must contain
/// CriticalMiss for the caster; must NOT contain DamageResult.
#[test]
fn cast_crit_fail_miss_emits_critical_miss_log_entry() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use storyforge::core::ResourceKind;

    let caster_pos = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("cf_miss_ability");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "CF Miss".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        effect: EffectDef::None,
        costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
        cost_ap: 1,
        aoe: AoEShape::None,
        friendly_fire: false,
        statuses: vec![],
        magic_domains: vec![],
        magic_method: String::new(),
        key: None,
        ai_tags_override: None,
    };

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, test_stats(), 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let target = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_hex);

    init_bridge_engine_state(&mut app);

    // Set mana and ensure AP is available.
    let caster_uid = entity_to_uid(caster);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(caster_uid).unwrap();
        unit.mana = Some((10, 10));
        unit.action_points = 2;
    }

    // Script d20=1 to force crit-fail.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[1]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: caster, ability: ability_id, target, target_pos: target_hex });

    app.update();

    let log = app.world().resource::<CombatLog>();

    // Must contain CriticalMiss for the caster.
    let crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { actor: a } if *a == caster));
    assert!(crit_miss, "CombatLog must contain CriticalMiss for the caster; got: {:?}", log.0);

    // Must NOT contain DamageResult (miss means no normal damage).
    let has_damage = log.0.iter().any(|e| matches!(e, CombatEvent::DamageResult { .. }));
    assert!(!has_damage, "CombatLog must NOT contain DamageResult on crit-fail miss; got: {:?}", log.0);
}

/// When d20 ≠ 1, CombatLog has NO CriticalMiss and NO CritFailSideEffect.
#[test]
fn cast_no_crit_fail_no_crit_fail_log_when_d20_non_one() {
    use storyforge::content::abilities::TargetType;

    let caster_pos = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("cf_normal_ability");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "CF Normal".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        // 0d1+5 = constant 5; only one random draw (the crit-fail d20).
        effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) },
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

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    let zero_str_stats = CombatStats { max_hp: 20, strength: 0, dexterity: 5, constitution: 10, intelligence: 0, wisdom: 10, charisma: 10 };
    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, zero_str_stats, 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();
    let target = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy, test_stats(), 0, 6, vec![],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(target, target_hex);

    init_bridge_engine_state(&mut app);

    let caster_uid = entity_to_uid(caster);
    app.world_mut().resource_mut::<CombatStateRes>().0.unit_mut(caster_uid).unwrap().action_points = 2;

    // Script d20=11 (no crit-fail); no further random draws for 0d1+5.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: caster, ability: ability_id, target, target_pos: target_hex });

    app.update();

    let log = app.world().resource::<CombatLog>();

    let has_crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { .. }));
    let has_crit_side = log.0.iter().any(|e| matches!(e, CombatEvent::CritFailSideEffect { .. }));

    assert!(!has_crit_miss, "CombatLog must NOT contain CriticalMiss when d20≠1; got: {:?}", log.0);
    assert!(!has_crit_side, "CombatLog must NOT contain CritFailSideEffect when d20≠1; got: {:?}", log.0);
}

// TODO(unisim phase2 step 7-followup or step 9): once EcsContentView populates
// crit_fail_outcome from race content (currently defaults to Miss), add bridge_smoke
// tests for CritFailSideEffect variants (DoubleCost, SelfDamage, ApplyStatus).
// Engine cast.rs tests already pin the per-outcome logic on the engine side.

// ── Phase 3.5c: Cast(Summon) creates ECS entity via bridge ───────────────────

/// Cast(Summon) → bridge creates a new ECS Combatant entity synchronously.
///
/// Verifies that:
/// 1. A new `Combatant` entity exists after process_action_system runs.
/// 2. The new entity is registered in `UnitIdMap`.
/// 3. The new entity's `HexPositions` entry is populated adjacent to the summoner.
/// 4. `SummonedBy(summoner)` component is set on the new entity.
/// 5. `CombatLog` contains a `Summoned` entry.
#[test]
fn cast_summon_creates_ecs_entity_synchronously() {
    use storyforge::content::abilities::TargetType;
    use storyforge::content::unit_templates::{EquipmentBlock, ResourcesBlock, UnitTemplateDef};
    use storyforge::game::components::{Combatant, SummonedBy};

    let summoner_pos = hex_from_offset(0, 0);

    let ability_id = AbilityId::from("summon_imp");
    let template_id = "imp";

    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Призвать беса".into(),
        target_type: TargetType::Myself,
        range: AbilityRange { min: 0, max: 0 },
        effect: EffectDef::Summon {
            template: template_id.into(),
            max_active: None,
        },
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

    let template = UnitTemplateDef {
        id: template_id.into(),
        name: "Imp".into(),
        race: String::new(),
        faction: None,
        path: None,
        speed: 4,
        stats: CombatStats { max_hp: 8, strength: 2, dexterity: 5, constitution: 8, intelligence: 0, wisdom: 5, charisma: 5 },
        equipment: EquipmentBlock {
            main_hand: "unarmed".into(),
            off_hand: None,
            chest: "".into(),
            legs: "".into(),
            feet: "".into(),
        },
        resources: ResourcesBlock::default(),
        ability_ids: vec![],
        ai_tuning_override: None,
    };

    let mut app = bridge_app();
    {
        let mut content = app.world_mut().resource_mut::<ActiveContent>();
        content.0.abilities.insert(ability_id.clone(), ability_def);
        content.0.unit_templates.insert(template_id.into(), template);
    }

    let summoner = app.world_mut().spawn(CombatantBundle::new(
        Team::Enemy,
        test_stats(),
        0,
        4,
        vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(summoner, summoner_pos);
    init_bridge_engine_state(&mut app);

    // Ensure summoner has AP.
    let summoner_uid = entity_to_uid(summoner);
    app.world_mut().resource_mut::<CombatStateRes>().0.unit_mut(summoner_uid).unwrap().action_points = 1;

    // Script crit-fail d20 to non-1 (summon has no damage roll after that).
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    // Cast summon targeting self (summoner == target for Myself abilities).
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast { actor: summoner, ability: ability_id, target: summoner, target_pos: summoner_pos });

    app.update();

    // Assert: a new Combatant entity exists (besides the summoner).
    let combatants: Vec<Entity> = app.world_mut()
        .query::<(Entity, &Combatant)>()
        .iter(app.world())
        .map(|(e, _)| e)
        .filter(|&e| e != summoner)
        .collect();
    assert_eq!(combatants.len(), 1, "expected exactly one summoned entity, got {:?}", combatants);
    let summoned = combatants[0];

    // Assert: registered in UnitIdMap.
    let id_map = app.world().resource::<UnitIdMap>();
    assert!(id_map.get_id(summoned).is_some(), "summoned entity must be in UnitIdMap");

    // Assert: has a position adjacent to summoner.
    let positions = app.world().resource::<HexPositions>();
    let pos = positions.get(&summoned).expect("summoned entity must have a HexPositions entry");
    assert_ne!(pos, summoner_pos, "summoned entity must not share summoner's hex");

    // Assert: SummonedBy component set.
    let summoned_by = app.world().entity(summoned).get::<SummonedBy>()
        .expect("summoned entity must have SummonedBy component");
    assert_eq!(summoned_by.0, summoner);

    // Assert: CombatLog has Summoned entry.
    let log = app.world().resource::<CombatLog>();
    let has_summoned = log.0.iter().any(|e| matches!(e, CombatEvent::Summoned { summoner: s, .. } if *s == summoner));
    assert!(has_summoned, "CombatLog must contain Summoned entry; got: {:?}", log.0);
}

/// Phase transition via `Action::Cast`: bridge writes ECS-only deltas and emits
/// `CombatEvent::PhaseEntered`.
///
/// Setup:
/// - Boss: max_hp=100, 1 pending phase at 50% threshold, heal_to_full=true, new_name="Phase Two".
/// - Caster fires a 0d1+60 damage spell (constant 60 damage, no armor).
/// - After step: boss hp crosses 50 → engine emits `Event::PhaseEntered`.
/// - Bridge should: pop `EnemyPhases.pending`, rename boss to "Phase Two",
///   heal to full (hp == max_hp), and push `CombatEvent::PhaseEntered`.
#[test]
fn phase_transition_via_cast_writes_ecs_and_emits_log_entry() {
    use storyforge::content::abilities::TargetType;
    use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
    use bevy::prelude::Name as BevyName;
    use storyforge::game::components::EnemyPhases;

    let caster_pos = hex_from_offset(0, 0);
    let boss_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("phase_nuke");
    // 0d1+60 → constant 60 damage, strength=0 so str_mod=0, boss armor=0.
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Phase Nuke".into(),
        target_type: TargetType::SingleEnemy,
        range: AbilityRange { min: 0, max: 5 },
        effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 60) },
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

    let mut app = bridge_app();
    app.world_mut().resource_mut::<ActiveContent>().0.abilities.insert(ability_id.clone(), ability_def);

    // Caster: str=0, no armor.
    let zero_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let caster = app.world_mut().spawn(CombatantBundle::new(
        Team::Player, zero_stats, 0, 6, vec![ability_id.clone()],
        Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
    )).id();

    // Boss: max_hp=100, armor=0. Pending phase at 50% threshold.
    let boss_stats = CombatStats {
        max_hp: 100, strength: 5, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let phase = PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(50),
        name: Some("Phase Two".into()),
        stats: None,
        ability_ids: None,
        heal_to_full: true,
        flavor: Some("Boss enters phase two!".into()),
    };
    let boss = app.world_mut().spawn((
        CombatantBundle::new(
            Team::Enemy, boss_stats, 0, 6, vec![],
            Equipment { main_hand: None, off_hand: None, chest: "".into(), legs: "".into(), feet: "".into() },
        ),
        EnemyPhases { pending: vec![phase] },
        BevyName::new("Boss"),
    )).id();

    app.world_mut().resource_mut::<HexPositions>().insert(caster, caster_pos);
    app.world_mut().resource_mut::<HexPositions>().insert(boss, boss_hex);

    init_bridge_engine_state(&mut app);

    // Give caster 1 AP (default); boss has 100 hp initially.
    // Script d20 to 11 so crit-fail doesn't fire.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[11]);

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast {
            actor: caster,
            ability: ability_id,
            target: boss,
            target_pos: boss_hex,
        });

    app.update();

    // --- Assertions ---

    // 1. EnemyPhases.pending is empty (pop happened).
    let phases = app.world().entity(boss).get::<EnemyPhases>()
        .expect("boss must retain EnemyPhases component");
    assert!(
        phases.pending.is_empty(),
        "EnemyPhases.pending must be empty after phase transition; got: {:?}",
        phases.pending,
    );

    // 2. Boss Name == "Phase Two" (ECS-only delta was written).
    let name = app.world().entity(boss).get::<BevyName>()
        .expect("boss must have Name");
    assert_eq!(name.as_str(), "Phase Two", "boss name must update to new phase name");

    // 3. Boss is alive (heal_to_full: engine revived, Dead was not inserted).
    let vital = app.world().entity(boss).get::<Vital>()
        .expect("boss must have Vital");
    assert!(vital.is_alive(), "boss must be alive after phase transition (heal_to_full=true)");
    assert_eq!(vital.hp, vital.max_hp, "boss must be healed to full after phase transition");

    // 4. CombatLog contains PhaseEntered with correct prev/next name.
    let log = app.world().resource::<CombatLog>();
    let phase_entry = log.0.iter().find_map(|e| {
        if let CombatEvent::PhaseEntered { actor, prev_name, next_name, flavor } = e {
            Some((*actor, prev_name.clone(), next_name.clone(), flavor.clone()))
        } else {
            None
        }
    });
    let (pe_actor, pe_prev, pe_next, pe_flavor) = phase_entry
        .expect("CombatLog must contain PhaseEntered; full log: {log:?}");
    assert_eq!(pe_actor, boss, "PhaseEntered.actor must be the boss entity");
    assert_eq!(pe_prev, "Boss", "PhaseEntered.prev_name must be original boss name");
    assert_eq!(pe_next, "Phase Two", "PhaseEntered.next_name must be new phase name");
    assert_eq!(pe_flavor, Some("Boss enters phase two!".into()), "PhaseEntered.flavor must match");
}
