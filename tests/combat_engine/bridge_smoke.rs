//! Smoke tests for the Bevy ↔ combat-engine bridge.
//!
//! Test 1 (`process_action_move_writes_engine_state_and_projects_to_ecs`):
//!   Verifies the full round-trip for a Move action:
//!   1. `bootstrap_combat_state` populates `CombatStateRes` from ECS (called via helper).
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

use storyforge::combat::engine_bridge::{entity_to_uid, CombatStateRes, UnitIdMap};
use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef};
use storyforge::content::content_view::ActiveContent;
use storyforge::content::statuses::StatusDef;
use storyforge::combat::DiceRngRes;
use combat_engine::{AbilityId, DiceExpr, StatusId, WeaponId};
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    ActionPoints, ActiveStatus, BonusMovement, CombatStats, Reactions, StatusEffects,
    Team, UnitToken, Vital,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;
use storyforge::ui::animation::{AnimationQueue, PendingAnim};

use super::common;


#[test]
fn process_action_move_writes_engine_state_and_projects_to_ecs() {
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0); // direct neighbor — costs 1 MP

    let mut app = common::apps::bridge::bridge_app();
    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);
    common::apps::bridge::bootstrap(&mut app);

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
    let player_start = hex_from_offset(0, 0);
    let enemy_pos = player_start.all_neighbors()[0];
    let escape_hex = player_start
        .all_neighbors()
        .into_iter()
        .find(|&h| h.unsigned_distance_to(enemy_pos) > 1)
        .expect("at least one non-adjacent neighbor must exist");

    let ability_id = AbilityId::from("test_attack");
    let weapon_id = WeaponId::from("test_sword");

    let cv = common::apps::bridge::melee_content(&ability_id, &weapon_id).into_view();

    let mut app = common::apps::bridge::bridge_app();
    app.insert_resource(ActiveContent(cv));

    let player = common::apps::bridge::spawn_caster(&mut app, player_start, vec![]);
    let enemy = common::apps::bridge::spawn_enemy_with_weapon(
        &mut app,
        enemy_pos,
        vec![ability_id],
        weapon_id,
    );
    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<Reactions>()
        .unwrap()
        .remaining = 1;

    common::apps::bridge::bootstrap(&mut app);

    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;

    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
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

    let ability_id = AbilityId::from("test_attack_stunned");
    let weapon_id = WeaponId::from("test_sword_stunned");
    let stun_id = StatusId::from("test_stun");

    let stun_def = StatusDef {
        id: stun_id.clone(),
        name: "Test Stun".into(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: storyforge::combat_engine::StatusDef {
            bonuses: storyforge::combat_engine::StatusBonuses::default(),
            skips_turn: true,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            causes_disadvantage: false,
        },
    };

    let cv = common::apps::bridge::melee_content(&ability_id, &weapon_id)
        .with_status(stun_def)
        .into_view();

    let mut app = common::apps::bridge::bridge_app();
    app.insert_resource(ActiveContent(cv));

    let player = common::apps::bridge::spawn_caster(&mut app, player_start, vec![]);
    let enemy = common::apps::bridge::spawn_enemy_with_weapon(
        &mut app,
        enemy_pos,
        vec![ability_id],
        weapon_id,
    );
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

    common::apps::bridge::bootstrap(&mut app);

    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;
    let enemy_reactions_before = app.world().entity(enemy).get::<Reactions>().unwrap().remaining;

    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
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
    let content = common::apps::bridge::melee_content(&ability_id, &weapon_id).into_view();

    let mut app = common::apps::bridge::bridge_app();
    app.insert_resource(ActiveContent(content));

    let player = common::apps::bridge::spawn_caster(&mut app, player_start, vec![]);
    let enemy = common::apps::bridge::spawn_enemy_with_weapon(&mut app, enemy_pos, vec![ability_id], weapon_id);
    app.world_mut().entity_mut(enemy).get_mut::<Reactions>().unwrap().remaining = 1;

    common::apps::bridge::bootstrap(&mut app);
    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
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

#[test]
fn engine_emits_combat_log_unit_moved() {
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();
    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);
    common::apps::bridge::bootstrap(&mut app);
    common::apps::bridge::write_move(&mut app, actor, vec![target]);
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

#[test]
fn engine_enqueues_movement_animation() {
    let start = hex_from_offset(0, 0);
    let step1 = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();
    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);
    // Spawn a token entity pointing at the actor.
    let token_entity = app.world_mut().spawn(UnitToken(actor)).id();
    common::apps::bridge::bootstrap(&mut app);

    let path = vec![step1];
    common::apps::bridge::write_move(&mut app, actor, path.clone());
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

#[test]
fn projector_removes_bonus_movement_when_mp_zero() {
    let start = hex_from_offset(0, 0);
    let target = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();

    // Spawn actor with movement_points = 1 (just enough for a 1-step path).
    let actor = common::apps::bridge::spawn_caster_with_speed(&mut app, start, vec![], 1);
    app.world_mut().entity_mut(actor).insert(BonusMovement);
    common::apps::bridge::bootstrap(&mut app);
    common::apps::bridge::write_move(&mut app, actor, vec![target]);
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
    let cv = common::apps::bridge::melee_content(&ability_id, &weapon_id).into_view();

    let mut app = common::apps::bridge::bridge_app();
    app.insert_resource(ActiveContent(cv));

    // Player with hp=1 — any hit is lethal.
    let weak_stats = CombatStats { max_hp: 1, strength: 5, dexterity: 5, constitution: 10, intelligence: 0, wisdom: 10, charisma: 10 };
    let player = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        weak_stats,
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        player_start,
    );
    let enemy = common::apps::bridge::spawn_enemy_with_weapon(
        &mut app,
        enemy_pos,
        vec![ability_id],
        weapon_id,
    );
    app.world_mut().entity_mut(enemy).get_mut::<Reactions>().unwrap().remaining = 1;

    // Script the dice: roll maximum damage (6) to guarantee a kill on hp=1.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[6]);

    common::apps::bridge::bootstrap(&mut app);

    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
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

    let mut seed_app = common::apps::bridge::bridge_app();

    common::apps::bridge::spawn_caster(&mut seed_app, start, vec![]);

    // Seed engine state from ECS (no mirror system in bridge_app).
    common::apps::bridge::bootstrap(&mut seed_app);

    // --- Phase B: set up projector-only app with the same entity / resources ---
    let mut app = common::apps::bridge::projector_only_app();

    // Spawn the same actor entity in the new world (entity id is stable across
    // App instances; we need the same Entity bits so UnitIdMap lookups work).
    let new_actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            common::apps::bridge::bridge_stats(),
            0,
            6,
            vec![],
            common::apps::bridge::default_equipment(),
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
            damage_taken_bonus: 0,
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
            pools: combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Mana   => None,
                combat_engine::PoolKind::Rage   => None,
                combat_engine::PoolKind::Energy => None,
                combat_engine::PoolKind::Ap     => Some((2, 2)),
                combat_engine::PoolKind::Mp     => Some((6, 6)),
            },
            regen_per_pool: combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
                combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
            },
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
        unit.pools[combat_engine::PoolKind::Mp] = Some((target_mp, target_mp));
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

/// Run a full cast round-trip and invoke `assert_log` on the resulting `CombatLog`.
///
/// `stats` is used for the caster. `setup_unit` is called after bootstrap to
/// mutate the engine unit (e.g. set mana). All other setup is identical across
/// the three "cast emits …" tests.
fn run_cast_log_test(
    ability: AbilityDef,
    caster_stats: CombatStats,
    setup_unit: impl FnOnce(&mut combat_engine::state::Unit),
    assert_log: impl FnOnce(&CombatLog),
) {
    let ability_id = ability.id.clone();
    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability);

    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        caster_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let target = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    let caster_uid = entity_to_uid(caster);
    app.world_mut()
        .resource_mut::<CombatStateRes>()
        .0
        .unit_mut(caster_uid)
        .unwrap()
        .action_points = 2;

    setup_unit(
        app.world_mut()
            .resource_mut::<CombatStateRes>()
            .0
            .unit_mut(caster_uid)
            .unwrap(),
    );

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, target, target_pos);
    app.update();

    assert_log(app.world().resource::<CombatLog>());

    let _ = target; // silence unused-variable warning
}



/// Cast with `EffectDef::Damage` emits `AbilityUsed` + `DamageResult` in CombatLog.
///
/// Caster has strength=0 (str_mod=0) so damage = dice bonus only (5).
/// Target has armor=0, so armor_reduced=0 and final_damage=5.
/// The crit-fail d20 is scripted to 11 (non-1) to ensure normal resolution.
#[test]
fn cast_emits_damage_result_log_entry() {
    use storyforge::content::abilities::TargetType;

    let zero_str_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let ability_def = AbilityDef {
        id: AbilityId::from("dmg_ability"),
        name: "Fireball".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) },
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };

    run_cast_log_test(ability_def, zero_str_stats, |_| {}, |log| {
        let ability_used: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::AbilityUsed { actor: a, ability_name, target: t, is_aoe, .. } = e {
                Some((*a, ability_name.clone(), *t, *is_aoe))
            } else { None }
        }).collect();
        assert_eq!(ability_used.len(), 1, "expected exactly one AbilityUsed, got {:?}", ability_used);
        let (_, au_name, _, au_aoe) = &ability_used[0];
        assert_eq!(au_name, "Fireball");
        assert!(!au_aoe, "ability is not AoE");

        let dmg_results: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::DamageResult { target: t, final_damage, armor_reduced, .. } = e {
                Some((*t, *final_damage, *armor_reduced))
            } else { None }
        }).collect();
        assert_eq!(dmg_results.len(), 1, "expected exactly one DamageResult, got {:?}", dmg_results);
        let (_, dr_dmg, dr_armor) = dmg_results[0];
        assert_eq!(dr_dmg, 5, "final_damage must be 5 (0d1+5, str_mod=0, armor=0)");
        assert_eq!(dr_armor, 0, "armor_reduced must be 0");
    });
}

/// Cast with status-only ability emits `AbilityUsed` + `StatusApplied` in CombatLog.
///
/// The ability has `EffectDef::None` and one status on the target.
#[test]
fn cast_emits_status_applied_log_entry() {
    use storyforge::content::abilities::{StatusApplication, StatusOn, TargetType};

    let status_id = StatusId::from("burning");
    let ability_def = AbilityDef {
        id: AbilityId::from("burning_touch"),
        name: "Burning Touch".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication { status: status_id.clone(), duration_rounds: 2, on: StatusOn::Target }],
            key: None,
        },
    };

    run_cast_log_test(ability_def, common::apps::bridge::bridge_stats(), |_| {}, |log| {
        let status_events: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::StatusApplied { target: t, status } = e {
                Some((*t, status.clone()))
            } else { None }
        }).collect();
        assert_eq!(status_events.len(), 1, "expected exactly one StatusApplied, got {:?}", status_events);
        let (_, ev_status) = &status_events[0];
        assert_eq!(*ev_status, status_id, "StatusApplied status must be 'burning'");
    });
}

/// Cast with mana cost emits `ManaChanged` in CombatLog.
///
/// Ability costs 3 mana; caster starts with mana=(10,10).
/// After cast: mana=(7,10) → bridge diff emits ManaChanged.
#[test]
fn cast_emits_mana_changed_log_entry() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let ability_def = AbilityDef {
        id: AbilityId::from("mana_blast"),
        name: "Mana Blast".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
            cost_ap: 0,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };

    run_cast_log_test(ability_def, common::apps::bridge::bridge_stats(), |unit| {
        unit.mana = Some((10, 10));
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
    }, |log| {
        let mana_events: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::ManaChanged { actor: a, current, max } = e {
                Some((*a, *current, *max))
            } else { None }
        }).collect();
        assert_eq!(mana_events.len(), 1, "expected exactly one ManaChanged, got {:?}", mana_events);
        let (_, mc_current, mc_max) = mana_events[0];
        assert_eq!(mc_current, 7, "mana after cast must be 10 - 3 = 7");
        assert_eq!(mc_max, 10, "mana max must be 10");
    });
}

// ── Phase 2 step 7a: ActionInput::Cast routing smoke test ────────────────────

#[test]
fn process_action_system_routes_cast_into_engine() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let mut app = common::apps::bridge::bridge_app();

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec!["zap".into()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let target = common::apps::bridge::spawn_target(&mut app, target_pos);

    // Register a Cast-able ability with a mana cost in ActiveContent.
    let zap_id = AbilityId::from("zap");
    let zap_def = AbilityDef {
        id: zap_id.clone(),
        name: "zap".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            // No damage in 7a — just verify cost flows through
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
        },
    };
    common::apps::bridge::insert_ability(&mut app, zap_def);

    common::apps::bridge::bootstrap(&mut app);

    // CombatantBundle default AP=1; bump to 2 so post-cast AP=1 is observable.
    // Mana isn't a default Bevy component on CombatantBundle — set on engine
    // state directly so PayCost has a pool to deduct from.
    let caster_uid = entity_to_uid(caster);
    common::apps::bridge::with_engine_unit(&mut app, caster, |unit| {
        unit.action_points = 2;
        unit.pools[combat_engine::PoolKind::Ap] = Some((2, unit.max_ap));
        unit.mana = Some((10, 10));
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
    });

    common::apps::bridge::write_cast(&mut app, caster, zap_id, target, target_pos);

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
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);

    // Add Mana component — not part of CombatantBundle by default.
    app.world_mut()
        .entity_mut(actor)
        .insert(Mana { current: 10, max: 10 });

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
        damage_taken_bonus: 0,
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
        pools: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => None,
            combat_engine::PoolKind::Rage   => None,
            combat_engine::PoolKind::Energy => None,
            combat_engine::PoolKind::Ap     => Some((1, 1)),
            combat_engine::PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
    };
    let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Mutate engine mana to simulate a cast that spent mana.
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.mana = Some((4, 10));
        u.pools[combat_engine::PoolKind::Mana] = Some((4, 10));
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
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);

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
        damage_taken_bonus: 0,
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
        pools: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => None,
            combat_engine::PoolKind::Rage   => None,
            combat_engine::PoolKind::Energy => None,
            combat_engine::PoolKind::Ap     => Some((1, 1)),
            combat_engine::PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
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
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);
    let aura_source = common::apps::bridge::spawn_caster(&mut app, start2, vec![]);

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
        damage_taken_bonus: 0,
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
        pools: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => None,
            combat_engine::PoolKind::Rage   => None,
            combat_engine::PoolKind::Energy => None,
            combat_engine::PoolKind::Ap     => Some((1, 1)),
            combat_engine::PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
    };
    let aura_unit = Unit {
        id: aura_source_uid,
        team: EngineTeam::Player,
        pos: start2,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
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
        pools: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => None,
            combat_engine::PoolKind::Rage   => None,
            combat_engine::PoolKind::Energy => None,
            combat_engine::PoolKind::Ap     => Some((1, 1)),
            combat_engine::PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
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

/// Run a cast round-trip with a scripted d20 value and assert whether `CriticalMiss`
/// appears in the log.
///
/// `expect_crit_fail=true`  → log must contain `CriticalMiss`, must NOT contain `DamageResult`.
/// `expect_crit_fail=false` → log must NOT contain `CriticalMiss` or `CritFailSideEffect`.
fn run_crit_fail_log_test(d20: i32, expect_crit_fail: bool) {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let ability_id = AbilityId::from("cf_test_ability");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "CF Test".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            // Use damage for d20≠1 path so the cast has a visible effect; use None for d20=1 (miss).
            effect: if expect_crit_fail {
                EffectDef::None
            } else {
                EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) }
            },
            costs: if expect_crit_fail {
                vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }]
            } else {
                vec![]
            },
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);
    let zero_str_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        zero_str_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let target = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    let caster_uid = entity_to_uid(caster);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(caster_uid).unwrap();
        unit.mana = Some((10, 10));
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
        unit.action_points = 2;
    }

    common::apps::bridge::script_d20(&mut app, d20);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, target, target_pos);
    app.update();

    let log = app.world().resource::<CombatLog>();

    if expect_crit_fail {
        let crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { actor: a } if *a == caster));
        assert!(crit_miss, "CombatLog must contain CriticalMiss for the caster; got: {:?}", log.0);

        let has_damage = log.0.iter().any(|e| matches!(e, CombatEvent::DamageResult { .. }));
        assert!(!has_damage, "CombatLog must NOT contain DamageResult on crit-fail miss; got: {:?}", log.0);
    } else {
        let has_crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { .. }));
        let has_crit_side = log.0.iter().any(|e| matches!(e, CombatEvent::CritFailSideEffect { .. }));
        assert!(!has_crit_miss, "CombatLog must NOT contain CriticalMiss when d20≠1; got: {:?}", log.0);
        assert!(!has_crit_side, "CombatLog must NOT contain CritFailSideEffect when d20≠1; got: {:?}", log.0);
    }

    let _ = target;
}

/// Bridge translates `Event::CritFailed { outcome: Miss }` → `CombatEvent::CriticalMiss`.
///
/// DiceRng scripted to 1 (crit-fail). After update: CombatLog must contain
/// CriticalMiss for the caster; must NOT contain DamageResult.
#[test]
fn cast_crit_fail_miss_emits_critical_miss_log_entry() {
    run_crit_fail_log_test(1, true);
}

/// When d20 ≠ 1, CombatLog has NO CriticalMiss and NO CritFailSideEffect.
#[test]
fn cast_no_crit_fail_no_crit_fail_log_when_d20_non_one() {
    run_crit_fail_log_test(11, false);
}

// TODO(unisim phase2 step 7-followup or step 9): once EcsContentView populates
// crit_fail_outcome from race content (currently defaults to Miss), add bridge_smoke
// tests for CritFailSideEffect variants (DoubleCost, SelfDamage, ApplyStatus).
// Engine cast.rs tests already pin the per-outcome logic on the engine side.

// ── Phase 3.5c: Cast(Summon) creates ECS entity via bridge ───────────────────

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
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template_id: template_id.into(),
                max_active: None,
            },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
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

    let mut app = common::apps::bridge::bridge_app();
    {
        let mut content = app.world_mut().resource_mut::<ActiveContent>();
        content.0.abilities.insert(ability_id.clone(), ability_def);
        content.0.unit_templates.insert(template_id.into(), template);
    }

    let summoner = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        4,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        summoner_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    // Ensure summoner has AP.
    common::apps::bridge::with_engine_unit(&mut app, summoner, |unit| {
        unit.action_points = 1;
    });

    // Script crit-fail d20 to non-1 (summon has no damage roll after that).
    common::apps::bridge::script_no_crit_fail(&mut app);

    // Cast summon targeting self (summoner == target for Myself abilities).
    common::apps::bridge::write_cast(&mut app, summoner, ability_id, summoner, summoner_pos);

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
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 60) },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

    // Caster: str=0 so str_mod=0, damage is purely from the +60 bonus.
    let zero_str_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        zero_str_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );

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
            common::apps::bridge::no_equipment(),
        ),
        EnemyPhases { pending: vec![phase] },
        BevyName::new("Boss"),
    )).id();
    app.world_mut().resource_mut::<HexPositions>().insert(boss, boss_hex);

    common::apps::bridge::bootstrap(&mut app);

    // Script d20 to 11 so crit-fail doesn't fire.
    common::apps::bridge::script_no_crit_fail(&mut app);

    common::apps::bridge::write_cast(&mut app, caster, ability_id, boss, boss_hex);

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

// ── EngineTraceWriter smoke test (Phase 5 step 5d, gate #11 / #12 / #14) ─────

/// Verifies that `EngineTraceWriter` can open a file, write an `InitLine`
/// with a `session_id`, then write two `StepLine`s, and that the resulting
/// JSONL is parseable with correct field values.
#[test]
fn engine_trace_writer_init_and_step() {
    use combat_engine::action::Action;
    use combat_engine::state::UnitId;
    use combat_engine::trace::{parse_init, parse_step, InitLine, SCHEMA_VERSION};
    use hexx::Hex;
    use std::io::BufRead;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;

    // Use a temp path unique to this test run (epoch-ns suffix).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("engine_trace_smoke_{ts}.jsonl"));

    let mut writer = EngineTraceWriter::default();
    writer.open(&path).expect("open trace file");

    // Write init line.
    let init = InitLine {
        schema: SCHEMA_VERSION,
        session_id: "test_session".to_owned(),
        rng_seed: 0xDEAD_BEEF,
        units: vec![],
        next_synthetic_uid: 0,
        round: 1,
        phase: combat_engine::state::RoundPhase::ActorTurn,
        turn_queue: combat_engine::TurnQueue::default(),
        content_hash: "blake3:test".to_owned(),
    };
    writer.write_init(&init).expect("write init");

    // Write two step lines.
    let action0 = Action::Move {
        actor: UnitId(1),
        path: vec![Hex::new(0, 0), Hex::new(1, 0)],
    };
    let action1 = Action::EndTurn { actor: UnitId(1) };
    writer
        .write_step(&action0, &[], 0, "blake3:hash0".to_owned())
        .expect("write step 0");
    writer
        .write_step(&action1, &[], 0, "blake3:hash1".to_owned())
        .expect("write step 1");
    writer.close();

    // Parse the file back.
    let file = std::fs::File::open(&path).expect("open for read");
    let mut lines = std::io::BufReader::new(file).lines();

    // Line 1: InitLine.
    let line1 = lines.next().expect("line 1 missing").expect("io");
    let parsed_init = parse_init(&line1).expect("parse init");
    assert_eq!(parsed_init.session_id, "test_session");
    assert_eq!(parsed_init.rng_seed, 0xDEAD_BEEF);

    // Line 2: StepLine step=0.
    let line2 = lines.next().expect("line 2 missing").expect("io");
    let parsed_step0 = parse_step(&line2).expect("parse step 0");
    assert_eq!(parsed_step0.step, 0);
    assert!(matches!(parsed_step0.action, Action::Move { .. }));

    // Line 3: StepLine step=1.
    let line3 = lines.next().expect("line 3 missing").expect("io");
    let parsed_step1 = parse_step(&line3).expect("parse step 1");
    assert_eq!(parsed_step1.step, 1);
    assert!(matches!(parsed_step1.action, Action::EndTurn { .. }));

    assert!(lines.next().is_none(), "no extra lines");
    let _ = std::fs::remove_file(&path);
}

// ── Gate #14: end-to-end record + replay via bridge app ───────────────────────

/// End-to-end test: drives actions through the bridge app with the engine trace
/// writer active, then reads the produced `engine.jsonl` and verifies it can be
/// replayed in-process with byte-equal events, rng_calls, and state hashes.
///
/// Gate #14 from Phase 5 §7.
#[test]
fn engine_trace_full_combat_record_replay() {
    use combat_engine::state::CombatState;
    use combat_engine::trace::{parse_init, parse_step, post_state_hash_hex, SCHEMA_VERSION};
    use combat_engine::dice::DiceRng;
    use combat_engine::step::step as engine_step;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use std::io::BufRead;

    // Use a unique temp path.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("engine_trace_e2e_{ts}.jsonl"));

    // ── Build app ────────────────────────────────────────────────────────────
    let mut app = common::apps::bridge::bridge_app();

    let start_hex = hex_from_offset(0, 0);
    let step1_hex = hex_from_offset(1, 0);
    let step2_hex = hex_from_offset(2, 0);

    let actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            common::apps::bridge::bridge_stats(),
            0,  // armor
            6,  // speed
            vec![],
            common::apps::bridge::default_equipment(),
        ))
        .id();

    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(actor, start_hex);

    // Seed engine state.
    common::apps::bridge::bootstrap(&mut app);

    // ── Open the trace writer + write InitLine manually ───────────────────────
    {
        let mut trace_writer = app.world_mut().resource_mut::<EngineTraceWriter>();
        trace_writer.open(&path).expect("open trace file");
    }
    // Write InitLine from the current engine state.
    {
        use combat_engine::trace::{InitLine, SCHEMA_VERSION};
        let rng_seed = app.world().resource::<DiceRngRes>().0.seed();
        let init = {
            let state = &app.world().resource::<CombatStateRes>().0;
            InitLine {
                schema: SCHEMA_VERSION,
                session_id: "e2e_test".to_owned(),
                rng_seed,
                units: state.units().to_vec(),
                next_synthetic_uid: state.next_synthetic_uid(),
                round: state.round,
                phase: state.phase,
                turn_queue: state.turn_queue.clone(),
                content_hash: "blake3:e2e_test".to_owned(),
            }
        };
        app.world_mut()
            .resource_mut::<EngineTraceWriter>()
            .write_init(&init)
            .expect("write init line");
    }

    // ── Drive 3 actions through the bridge ───────────────────────────────────
    // Action 1: Move to step1_hex.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: vec![step1_hex] });
    app.update();

    // Action 2: Move to step2_hex.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path: vec![step2_hex] });
    app.update();

    // Action 3: EndTurn.
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::EndTurn { actor });
    app.update();

    // Close the trace writer.
    app.world_mut().resource_mut::<EngineTraceWriter>().close();

    // ── Read the produced engine.jsonl ────────────────────────────────────────
    let file = std::fs::File::open(&path).expect("open trace for read");
    let raw_lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect();

    assert!(
        raw_lines.len() >= 2,
        "expected at least InitLine + 1 StepLine, got {}",
        raw_lines.len()
    );

    // ── In-process replay ─────────────────────────────────────────────────────
    let parsed_init = parse_init(&raw_lines[0]).expect("parse init");
    assert_eq!(parsed_init.schema, SCHEMA_VERSION);

    // Reconstruct state from InitLine.
    let mut replay_state = {
        let mut s = CombatState::new(
            parsed_init.units.clone(),
            parsed_init.round,
            parsed_init.phase,
            parsed_init.rng_seed,
        );
        s.set_turn_queue(parsed_init.turn_queue.order.clone(), parsed_init.turn_queue.index);
        s.set_next_synthetic_uid(parsed_init.next_synthetic_uid);
        s
    };
    let mut replay_rng = DiceRng::with_seed(parsed_init.rng_seed);

    // Use empty content — the bridge test uses default DiceRngRes (ExpectedValue)
    // for determinism, and no ability content is needed for Move/EndTurn.
    use combat_engine::TomlContentView;
    let content = TomlContentView::empty();

    for (idx, line_str) in raw_lines[1..].iter().enumerate() {
        let recorded = parse_step(line_str)
            .unwrap_or_else(|e| panic!("parse step {idx}: {e}"));

        let (live_events, live_ctx) = engine_step(
            &mut replay_state,
            recorded.action.clone(),
            &mut replay_rng,
            &content,
        )
        .unwrap_or_else(|e| panic!("replay step {idx} failed: {e:?}"));

        assert_eq!(
            live_events, recorded.events,
            "step {idx}: events diverged"
        );
        assert_eq!(
            live_ctx.rng_calls, recorded.rng_calls,
            "step {idx}: rng_calls diverged (recorded={} live={})",
            recorded.rng_calls, live_ctx.rng_calls
        );
        let live_hash = post_state_hash_hex(&replay_state);
        assert_eq!(
            live_hash, recorded.post_state_hash,
            "step {idx}: post_state_hash diverged"
        );
    }

    let _ = std::fs::remove_file(&path);
}

// ── Gate #15: engine_step_range populated by deferred flush ──────────────────

/// Verifies Phase 6c: `engine_step_range` in AI log entries is populated with
/// the correct step-counter window `[start, end)` by `flush_pending_ai_log_system`.
///
/// Flow:
///   1. Open AiLogger + EngineTraceWriter to temp files.
///   2. Push one pending entry with start_step = 0 (trace counter before dispatch).
///   3. Drive a Move action through the bridge (step counter → 1).
///   4. flush_pending_ai_log_system runs (in the same chain as process_action_system).
///   5. Read the produced ai.jsonl line; assert engine_step_range == [0, 1].
#[test]
fn ai_log_engine_step_range_populated() {
    use storyforge::combat::ai::log::{AiLogger, PendingAiLogEntries};
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use std::io::BufRead;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ai_path = std::env::temp_dir().join(format!("ai_step_range_smoke_{ts}.jsonl"));
    let trace_path = std::env::temp_dir().join(format!("engine_step_range_trace_{ts}.jsonl"));

    let mut app = common::apps::bridge::bridge_app();

    // Spawn a combatant and seed engine state.
    let start_hex = hex_from_offset(0, 0);
    let target_hex = hex_from_offset(1, 0);
    let actor = common::apps::bridge::spawn_caster(&mut app, start_hex, vec![]);
    common::apps::bridge::bootstrap(&mut app);

    // Open both writers.
    app.world_mut()
        .resource_mut::<EngineTraceWriter>()
        .open(&trace_path)
        .expect("open trace file");
    app.world_mut()
        .resource_mut::<AiLogger>()
        .open(ai_path.clone())
        .expect("open ai log");

    // Verify step counter starts at 0.
    let step_before = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_before, 0, "step counter must start at 0");

    // Build a minimal actor_tick event (mimics what the AI system would push).
    // We push it directly into PendingAiLogEntries with start_step = 0.
    let fake_entry: storyforge::combat::ai::log::ActorTickEvent = serde_json::from_value(
        serde_json::json!({
            "event_type": "actor_tick",
            "schema_version": 36,
            "round": 1,
            "timestamp_ms": 0u64,
            "actor_id": 0u64,
            "actor_name": "test_actor",
            "snapshot": {"units": [], "round": 1},
            "plans": [],
            "decision": {"kind": "end_turn"}
        }),
    )
    .expect("test fixture parses as ActorTickEvent");
    app.world_mut()
        .resource_mut::<PendingAiLogEntries>()
        .entries
        .push((fake_entry, 0));

    // Dispatch Move — process_action_system advances step counter to 1,
    // then flush_pending_ai_log_system writes the entry with range [0, 1).
    common::apps::bridge::write_move(&mut app, actor, vec![target_hex]);
    app.update();

    // Step counter should now be 1 (one Move step was applied).
    let step_after = app.world().resource::<EngineTraceWriter>().step_counter();
    assert_eq!(step_after, 1, "step counter must be 1 after one Move");

    // Close writers.
    app.world_mut().resource_mut::<EngineTraceWriter>().close();
    app.world_mut().resource_mut::<AiLogger>().close();

    // Pending queue must be empty after flush.
    assert!(
        app.world().resource::<PendingAiLogEntries>().entries.is_empty(),
        "pending queue must be empty after flush"
    );

    // Read and verify the ai.jsonl line.
    let file = std::fs::File::open(&ai_path).expect("open ai log for read");
    let lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect();

    assert_eq!(lines.len(), 1, "expected exactly 1 actor_tick line");
    let v: serde_json::Value = serde_json::from_str(&lines[0]).expect("parse actor_tick json");

    let range = v.get("engine_step_range").expect("engine_step_range must be present");
    let arr = range.as_array().expect("engine_step_range must be an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].as_u64().unwrap(), 0, "start_step must be 0");
    assert_eq!(arr[1].as_u64().unwrap(), 1, "end_step must be 1");

    let _ = std::fs::remove_file(&ai_path);
    let _ = std::fs::remove_file(&trace_path);
}


// ── Phase B-α: lock-in tests (pre-S6 bridge contract) ─────────────────────────

/// Locks in the observable bridge contract for Cast-that-exhausts-AP/MP.
///
/// Today the bridge has a synchronous auto-end block that fires step(EndTurn)
/// when the caster's AP+MP hit 0 after a cast.  After B-γ (S6) the engine will
/// self-end its turn, but the bridge output observable to the UI must be
/// identical in both cases.  This test pins that contract.
///
/// Setup: hero casts an ability with cost_ap=1.  Hero starts with AP=1, MP=0
/// so after the cast AP=0, MP=0 → auto-end fires.
///
/// Assertions (must pass pre- AND post-S6):
///  - CombatLog contains, in order: AbilityUsed(hero), TurnEnded(hero), TurnStarted(enemy).
///  - ActiveCombatant migrated from hero to enemy.
#[test]
fn cast_via_bridge_exhausting_ap_mp_emits_turn_lifecycle_in_log() {
    use storyforge::content::abilities::TargetType;
    use storyforge::game::components::ActiveCombatant;

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();

    // Register a minimal cast ability (no damage needed — we just need AP cost).
    let ability_id = AbilityId::from("exhausting_zap");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Exhausting Zap".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };
    common::apps::bridge::insert_ability(&mut app, ability_def);

    let hero = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let enemy = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    // Set engine turn queue: hero is index=0 (current), enemy is index=1.
    // bootstrap() doesn't set the engine turn queue when the ECS TurnQueue is
    // empty (bridge tests don't run build_turn_order). Without this, step(EndTurn)
    // inside the auto-end block fails with NotCurrent and is silently swallowed.
    let hero_uid = entity_to_uid(hero);
    let enemy_uid = entity_to_uid(enemy);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        state.0.set_turn_queue(vec![hero_uid, enemy_uid], 0);
    }

    // Set hero: AP=1 (default is 1 from CombatantBundle), MP=0.
    // Bridge auto-end fires when AP<=0 && MP<=0 after cast.
    common::apps::bridge::with_engine_unit(&mut app, hero, |u| {
        u.action_points = 1;
        u.movement_points = 0;
    });

    // Insert ActiveCombatant on hero to simulate it being the active combatant.
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, hero, ability_id, enemy, target_pos);
    app.update();

    // ── Assert CombatLog order ────────────────────────────────────────────────
    let log = app.world().resource::<CombatLog>();

    // Find indices of the relevant events.
    let ability_used_idx = log.0.iter().position(|e| matches!(e, CombatEvent::AbilityUsed { .. }));
    let turn_ended_idx = log.0.iter().position(|e| {
        matches!(e, CombatEvent::TurnEnded { actor: a, .. } if *a == hero)
    });
    let turn_started_enemy_idx = log.0.iter().position(|e| {
        matches!(e, CombatEvent::TurnStarted { actor: a } if *a == enemy)
    });

    assert!(
        ability_used_idx.is_some(),
        "CombatLog must contain AbilityUsed; log: {:?}", log.0,
    );
    assert!(
        turn_ended_idx.is_some(),
        "CombatLog must contain TurnEnded(hero); log: {:?}", log.0,
    );
    assert!(
        turn_started_enemy_idx.is_some(),
        "CombatLog must contain TurnStarted(enemy); log: {:?}", log.0,
    );

    // Order: AbilityUsed → TurnEnded(hero) → TurnStarted(enemy).
    let au = ability_used_idx.unwrap();
    let te = turn_ended_idx.unwrap();
    let ts = turn_started_enemy_idx.unwrap();
    assert!(
        au < te && te < ts,
        "expected AbilityUsed[{au}] < TurnEnded[{te}] < TurnStarted[{ts}]; log: {:?}",
        log.0,
    );

    // ── Assert ActiveCombatant migrated hero → enemy ──────────────────────────
    // apply_pending_turn_lifecycle_system runs after process_action_system via
    // PendingTurnLifecycle.remove_active / insert_active queues.
    // After the full app.update(), ActiveCombatant should be on enemy, not hero.
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_some(),
        "enemy must have ActiveCombatant after turn handoff",
    );
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_none(),
        "hero must NOT have ActiveCombatant after turn handoff",
    );
}

/// Locks in the DoT applier semantics during Cast-that-exhausts-AP/MP.
///
/// Hero has a poison status applied to the enemy (applier=hero_uid).
/// `tick_actor_statuses` fires for statuses where `applier == starting_actor`,
/// so hero's poison-on-enemy ticks when HERO's turn starts (round 2+), NOT
/// when the turn hands off to the enemy.
///
/// This test pins the semantic so B-γ cannot accidentally invert it.
/// Must pass both pre- and post-S6.
#[test]
fn cast_with_dot_status_ticks_next_actor_dot_on_handoff() {
    use storyforge::content::abilities::TargetType;
    use storyforge::combat_engine::state::ActiveStatus as EngineActiveStatus;
    use storyforge::combat::engine_bridge::entity_to_uid;

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();

    // Register ability (AP=1, no damage, just to trigger exhaustion).
    let ability_id = AbilityId::from("final_strike");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Final Strike".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
        },
    };
    common::apps::bridge::insert_ability(&mut app, ability_def);

    // Register a poison StatusDef with hp_percent_dot=10 so ticking it would
    // deal damage.  If the DoT fires on handoff, enemy HP drops — and we assert
    // it does NOT drop.
    let poison_id = StatusId::from("hero_poison");
    app.world_mut()
        .resource_mut::<storyforge::content::content_view::ActiveContent>()
        .0
        .statuses
        .insert(poison_id.clone(), storyforge::content::statuses::StatusDef {
            id: poison_id.clone(),
            name: "Hero Poison".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                hp_percent_dot: 10,
                ..Default::default()
            },
        });

    let hero = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let enemy = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    let hero_uid = entity_to_uid(hero);
    let enemy_uid = entity_to_uid(enemy);

    // Set engine turn queue: hero is index=0 (current), enemy is index=1.
    // Required for step(EndTurn) in the bridge auto-end block to succeed.
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        state.0.set_turn_queue(vec![hero_uid, enemy_uid], 0);
    }

    // Record enemy HP before the cast.
    let enemy_hp_before = app
        .world()
        .resource::<CombatStateRes>()
        .0
        .unit(enemy_uid)
        .unwrap()
        .hp;

    // Inject hero's poison onto the enemy engine unit directly.
    // applier=hero_uid means it ticks at hero's turn start, NOT enemy's.
    common::apps::bridge::with_engine_unit(&mut app, enemy, |u| {
        u.statuses.push(EngineActiveStatus {
            id: poison_id.clone(),
            rounds_remaining: 3,
            dot_per_tick: 0,   // flat tick = 0; damage comes from hp_percent_dot in StatusDef
            applier: hero_uid,
        });
    });

    // Hero: AP=1, MP=0 → cast exhausts AP.
    common::apps::bridge::with_engine_unit(&mut app, hero, |u| {
        u.action_points = 1;
        u.movement_points = 0;
    });

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, hero, ability_id, enemy, target_pos);
    app.update();

    // Enemy HP must NOT have changed — hero's poison ticks at HERO's round-2
    // turn start, not at the handoff to enemy's turn.
    let enemy_hp_after = app
        .world()
        .resource::<CombatStateRes>()
        .0
        .unit(enemy_uid)
        .unwrap()
        .hp;

    assert_eq!(
        enemy_hp_after, enemy_hp_before,
        "enemy HP must be unchanged immediately after cast-and-handoff; \
         hero's poison (applier=hero) ticks at hero's turn start (round 2+), \
         not at enemy's turn start. before={enemy_hp_before}, after={enemy_hp_after}",
    );
}
