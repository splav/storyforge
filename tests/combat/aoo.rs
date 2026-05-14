#![allow(unused_imports)]

use bevy::prelude::*;

use crate::common::*;
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    Abilities, ActionPoints, ActiveCombatant, ActiveStatus, Dead, Rage, Reactions, StatusEffects, Vital,
};
use storyforge::game::hex::{hex_from_offset, Hex};
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::HexPositions;

fn aoo_events(app: &App) -> Vec<(Entity, i32, bool)> {
    app.world()
        .resource::<CombatLog>()
        .0
        .iter()
        .filter_map(|e| match e {
            CombatEvent::OpportunityAttack { attacker, damage, killed, .. } => {
                Some((*attacker, *damage, *killed))
            }
            _ => None,
        })
        .collect()
}

fn spawn_at(app: &mut App, pos: Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, pos);
    e
}

/// Heroes and goblin placed such that (3,3) and (4,3) are adjacent in even-r layout.
fn start_pos() -> Hex { hex_from_offset(3, 3) }
fn goblin_pos() -> Hex { hex_from_offset(4, 3) }
/// (2,3) is one hex left of hero; not adjacent to goblin at (4,3) — distance 2.
fn away_pos() -> Hex { hex_from_offset(2, 3) }

/// Baseline: covers trigger, armor mitigation (#9), rage gain on both sides (#11).
/// Also serves as the control case for `stunned_enemy_no_opportunity` (#10).
#[test]
fn leave_adjacent_triggers_aoo() {
    let mut app = movement_app();
    // Weapon 1d8 + STR_mod(2) = raw 2+2=4. Hero armor 3, status 0 → final = max(1, 4-3) = 1.
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[2]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().get_mut::<Vital>(hero).unwrap().armor = 3;
    app.world_mut().entity_mut(hero).insert(Rage::new(5));
    app.world_mut().entity_mut(goblin).insert(Rage::new(5));
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let hp_before = app.world().get::<Vital>(hero).unwrap().hp;

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();

    let events = aoo_events(&app);
    assert_eq!(events.len(), 1, "one AoO expected, got {events:?}");
    let (attacker, dmg, killed) = events[0];
    assert_eq!(attacker, goblin);
    assert!(!killed);
    assert_eq!(dmg, 1, "armor mitigation: 4 raw - 3 armor = 1");

    let hp_after = app.world().get::<Vital>(hero).unwrap().hp;
    assert_eq!(hp_after, hp_before - 1);
    assert_eq!(app.world().get::<Reactions>(goblin).unwrap().remaining, 0);
    assert_eq!(app.world().resource::<HexPositions>().get(&hero), Some(away_pos()));

    // Rage +1 on both sides (mirrors apply_effects behavior).
    assert_eq!(app.world().get::<Rage>(hero).unwrap().current, 1);
    assert_eq!(app.world().get::<Rage>(goblin).unwrap().current, 1);
}

#[test]
fn opportunity_once_per_round() {
    let mut app = movement_app();
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[3, 3]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let _goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Two separate ActionInput::Move events in the SAME round. Between them we manually restore
    // movement_points and hero position directly in CombatStateRes; we do NOT touch
    // reactions_left. Without a StartRound reset the second leave must not produce an AoO.
    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();
    assert_eq!(aoo_events(&app).len(), 1, "first move triggers AoO");

    {
        use storyforge::combat::engine_bridge::{entity_to_uid, CombatStateRes};
        let hero_uid = entity_to_uid(hero);
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(hero_uid).expect("hero in engine state");
        unit.movement_points = 10;
        unit.pos = start_pos();
        // DO NOT reset reactions_left — that's what the test is verifying.
    }
    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();
    assert_eq!(aoo_events(&app).len(), 1, "second move must not trigger — reaction spent");
}

#[test]
fn stunned_enemy_no_opportunity() {
    let mut app = movement_app();
    insert_stun_status(&mut app);
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[8]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut()
        .get_mut::<StatusEffects>(goblin)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "stun".into(),
            rounds_remaining: 1,
            applier: hero,
            dot_per_tick: 0,
        });
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();

    assert!(aoo_events(&app).is_empty(), "stunned enemy must not react");
}

/// Two provokers both adjacent; non-lethal hit → both fire (#3).
#[test]
fn multiple_provokers_all_fire() {
    let mut app = movement_app();
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[2, 2]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let g1 = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "G1");
    let g2 = spawn_at(&mut app, hex_from_offset(3, 4), test_enemy(base_stats()), "G2");
    // Sanity: both flank hero, both leave adjacency after move to (2,3).
    assert_eq!(start_pos().unsigned_distance_to(hex_from_offset(3, 4)), 1);
    assert_eq!(away_pos().unsigned_distance_to(hex_from_offset(3, 4)), 2);
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();

    let events = aoo_events(&app);
    assert_eq!(events.len(), 2, "both provokers fire, got {events:?}");
    let attackers: Vec<Entity> = events.iter().map(|(a, _, _)| *a).collect();
    assert!(attackers.contains(&g1) && attackers.contains(&g2));
}

/// Truncate on death (#4): two flankers, first AoO kills → second doesn't fire.
#[test]
fn dead_actor_truncates_path() {
    let mut app = movement_app();
    // First roll = 8 (lethal at hp=1, armor=0). Second roll scripted but should never fire.
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[8, 8]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let _g1 = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "G1");
    let _g2 = spawn_at(&mut app, hex_from_offset(3, 4), test_enemy(base_stats()), "G2");
    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 1;
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let path = vec![away_pos(), hex_from_offset(1, 3), hex_from_offset(0, 3)];
    write_message(&mut app, ActionInput::Move { actor: hero, path });
    app.update();

    let events = aoo_events(&app);
    assert_eq!(events.len(), 1, "second provoker must not fire after hero dies");
    assert!(events[0].2, "killed flag set");
    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        Some(away_pos()),
        "path truncated at step 0 (died during first leave)"
    );
    assert!(!app.world().get::<Vital>(hero).unwrap().is_alive());
    assert!(app.world().get::<Dead>(hero).is_some(), "Dead marker inserted");
    let log = app.world().resource::<CombatLog>();
    assert!(
        log.0.iter().any(|e| matches!(e, CombatEvent::UnitDied { entity } if *entity == hero)),
        "UnitDied event emitted for AoO kill"
    );
}

#[test]
fn no_melee_enemy_no_opportunity() {
    let mut app = movement_app();
    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().get_mut::<Abilities>(goblin).unwrap().0.clear();
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();

    assert!(aoo_events(&app).is_empty(), "enemy without melee must not react");
}

/// Faction symmetry (#1): enemy moves, hero provokes.
#[test]
fn enemy_mover_hero_provokes() {
    let mut app = movement_app();
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[5]);

    let hero = spawn_at(&mut app, goblin_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, start_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let hp_before = app.world().get::<Vital>(goblin).unwrap().hp;
    write_message(&mut app, ActionInput::Move { actor: goblin, path: vec![away_pos()] });
    app.update();

    let events = aoo_events(&app);
    assert_eq!(events.len(), 1, "hero should AoO the fleeing goblin, got {events:?}");
    assert_eq!(events[0].0, hero);
    assert!(app.world().get::<Vital>(goblin).unwrap().hp < hp_before);
}

/// Reactions refill on StartRound (#3): deplete via AoO, run build_turn_order, expect full again.
#[test]
fn reactions_refill_on_round_start() {
    use bevy::prelude::NextState;
    use storyforge::app_state::CombatPhase;

    let mut app = movement_app();
    app.world_mut().resource_mut::<storyforge::core::DiceRng>().script(&[3]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![away_pos()] });
    app.update();
    assert_eq!(app.world().get::<Reactions>(goblin).unwrap().remaining, 0);

    // Simulate round rollover: enter StartRound, let build_turn_order run.
    app.world_mut()
        .resource_mut::<NextState<CombatPhase>>()
        .set(CombatPhase::StartRound);
    app.update(); // transition + build_turn_order refills reactions and advances to AwaitCommand.

    let r = app.world().get::<Reactions>(goblin).unwrap();
    assert_eq!(r.remaining, r.max, "reaction should refill at StartRound");
}

#[test]
fn move_within_adjacency_no_trigger() {
    let mut app = movement_app();
    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Move to a cell still adjacent to goblin. (3,2) and (4,3) both neighbor hero.
    // Verify adjacency before asserting no trigger.
    let step = hex_from_offset(3, 2);
    assert_eq!(step.unsigned_distance_to(goblin_pos()), 1, "precondition: step still adjacent");

    write_message(&mut app, ActionInput::Move { actor: hero, path: vec![step] });
    app.update();

    assert!(aoo_events(&app).is_empty(), "stayed adjacent, no AoO");
    let r = app.world().get::<Reactions>(goblin).unwrap();
    assert_eq!(r.remaining, r.max);
}
