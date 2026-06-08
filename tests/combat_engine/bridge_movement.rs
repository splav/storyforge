//! Bridge smoke tests: movement, opportunity attacks, and AoO side-effects.
//!
//! Covers the full Move round-trip through `process_action_system` and
//! `project_state_to_ecs`, AoO dice routing from `EcsContentView`, the stunned-
//! enemy filter, combat-log emission for movement events, animation enqueueing,
//! bonus-movement removal, and Dead-marker insertion on lethal AoO.

use bevy::prelude::*;

use combat_engine::{AbilityId, StatusId, WeaponId};
use storyforge::combat::engine_bridge::{entity_to_uid, CombatStateRes};
use storyforge::combat::DiceRngRes;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::statuses::StatusDef;
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    ActionPoints, ActiveStatus, BonusMovement, CombatStats, Reactions, StatusEffects, Team,
    UnitToken, Vital,
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
        let unit = state
            .0
            .unit(actor_uid)
            .expect("actor must be in engine state after mirror");
        assert_eq!(
            unit.pos, start,
            "engine state should reflect actor start pos after mirror"
        );
    }

    // --- Send ActionInput::Move for the actor ---
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move {
            actor,
            path: vec![target],
        });

    // --- Update: process_action_system calls step(), project_state_to_ecs writes engine state back to ECS ---
    app.update();

    // Engine state: actor at target hex.
    let (engine_pos, engine_mp) = {
        let state = app.world().resource::<CombatStateRes>();
        let unit = state
            .0
            .unit(actor_uid)
            .expect("actor must still be in engine state");
        let mp = unit.pools[combat_engine::PoolKind::Mp]
            .map(|(c, _)| c)
            .unwrap_or(0);
        (unit.pos, mp)
    };
    assert_eq!(
        engine_pos, target,
        "engine state must show actor at target hex after step()"
    );
    assert_eq!(
        engine_mp, 5,
        "engine movement_points must be 6 - 1 = 5 after one-hex move"
    );

    // ECS: projector has written engine state back.
    let ecs_pos = app
        .world()
        .resource::<HexPositions>()
        .get(&actor)
        .expect("actor must still have an ECS position");
    assert_eq!(
        ecs_pos, target,
        "ECS HexPositions must be updated by project_state_to_ecs"
    );

    let ecs_mp = app
        .world()
        .entity(actor)
        .get::<ActionPoints>()
        .expect("actor must have ActionPoints")
        .movement_points;
    assert_eq!(
        ecs_mp, 5,
        "ECS movement_points must match engine after projection"
    );
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
/// Assertion: player's `Vital.hp()` is less than `max_hp` after two updates,
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
            heal_per_tick: 0,
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
    // applier: None (environment-applied) so tick_actor_statuses(player) at
    // turn-start does NOT expire the stun before the move happens.
    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(ActiveStatus {
            id: stun_id,
            rounds_remaining: 1,
            applier: None,
            dot_per_tick: 0,
        });

    common::apps::bridge::bootstrap(&mut app);

    let max_hp = app.world().entity(player).get::<Vital>().unwrap().max_hp;
    let enemy_reactions_before = app
        .world()
        .entity(enemy)
        .get::<Reactions>()
        .unwrap()
        .remaining;

    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
    app.update();

    let hp_after = app.world().entity(player).get::<Vital>().unwrap().hp;
    let enemy_reactions_after = app
        .world()
        .entity(enemy)
        .get::<Reactions>()
        .unwrap()
        .remaining;

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
    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
    app.update();

    let log = app.world().resource::<CombatLog>();
    let aoo_events: Vec<_> = log
        .0
        .iter()
        .filter_map(|e| {
            if let CombatEvent::OpportunityAttack {
                attacker,
                target,
                damage,
                killed,
            } = e
            {
                Some((*attacker, *target, *damage, *killed))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        aoo_events.len(),
        1,
        "exactly one OpportunityAttack expected, got {:?}",
        aoo_events
    );
    let (att, tgt, dmg, killed) = aoo_events[0];
    assert_eq!(att, enemy, "attacker must be the enemy");
    assert_eq!(tgt, player, "target must be the player");
    assert!(dmg > 0, "damage must be positive");
    assert!(
        !killed,
        "player should not die from one AoO with default HP"
    );
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
    let moved_events: Vec<_> = log
        .0
        .iter()
        .filter_map(|e| {
            if let CombatEvent::UnitMoved { actor: a, from, to } = e {
                Some((*a, *from, *to))
            } else {
                None
            }
        })
        .collect();

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
        other => panic!(
            "expected Movement animation, got {:?}",
            std::mem::discriminant(other)
        ),
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
    let weak_stats = CombatStats {
        max_hp: 1,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
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
    app.world_mut()
        .entity_mut(enemy)
        .get_mut::<Reactions>()
        .unwrap()
        .remaining = 1;

    // Script the dice: roll maximum damage (6) to guarantee a kill on hp=1.
    app.world_mut().resource_mut::<DiceRngRes>().script(&[6]);

    common::apps::bridge::bootstrap(&mut app);

    common::apps::bridge::write_move(&mut app, player, vec![escape_hex]);
    app.update();

    // Dead component must be inserted.
    assert!(
        app.world()
            .entity(player)
            .get::<storyforge::game::components::Dead>()
            .is_some(),
        "player must have Dead component after lethal AoO"
    );

    // CombatLog must contain UnitDied.
    let log = app.world().resource::<CombatLog>();
    let died = log
        .0
        .iter()
        .any(|e| matches!(e, CombatEvent::UnitDied { entity } if *entity == player));
    assert!(died, "CombatLog must contain UnitDied for the player");
}
