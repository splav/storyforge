use bevy::prelude::*;

use crate::common::{apps::engine::*, fixtures::*, scenarios::statuses::*};
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    Abilities, ActiveCombatant, ActiveStatus, Dead, Rage, Reactions, RuntimeStatsMirror,
    StatusEffects, Vital,
};
use storyforge::game::hex::{hex_from_offset, Hex};
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{HexCorpses, HexPositions};

fn aoo_events(app: &App) -> Vec<(Entity, i32, bool)> {
    app.world()
        .resource::<CombatLog>()
        .0
        .iter()
        .filter_map(|e| match e {
            CombatEvent::OpportunityAttack {
                attacker,
                damage,
                killed,
                ..
            } => Some((*attacker, *damage, *killed)),
            _ => None,
        })
        .collect()
}

fn spawn_at(app: &mut App, pos: Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos);
    e
}

/// Heroes and goblin placed such that (3,3) and (4,3) are adjacent in even-r layout.
fn start_pos() -> Hex {
    hex_from_offset(3, 3)
}
fn goblin_pos() -> Hex {
    hex_from_offset(4, 3)
}
/// (2,3) is one hex left of hero; not adjacent to goblin at (4,3) — distance 2.
fn away_pos() -> Hex {
    hex_from_offset(2, 3)
}

/// Baseline: covers trigger, armor mitigation (#9), rage gain on both sides (#11).
/// Also serves as the control case for `stunned_enemy_no_opportunity` (#10).
#[test]
fn leave_adjacent_triggers_aoo() {
    let mut app = movement_app();
    // Weapon 1d8 + STR_mod(2) = raw 2+2=4. Hero armor 3, status 0 → final = max(1, 4-3) = 1.
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[2]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut()
        .get_mut::<RuntimeStatsMirror>(hero)
        .unwrap()
        .0
        .armor = 3;
    app.world_mut().entity_mut(hero).insert(Rage::new(5));
    app.world_mut().entity_mut(goblin).insert(Rage::new(5));
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let hp_before = app.world().get::<Vital>(hero).unwrap().hp;

    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
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
    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        Some(away_pos())
    );

    // Rage +1 on both sides (mirrors apply_effects behavior).
    assert_eq!(app.world().get::<Rage>(hero).unwrap().current, 1);
    assert_eq!(app.world().get::<Rage>(goblin).unwrap().current, 1);
}

#[test]
fn opportunity_once_per_round() {
    let mut app = movement_app();
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[3, 3]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let _goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Two separate ActionInput::Move events in the SAME round. Between them we manually restore
    // pools[Mp] and hero position directly in CombatStateRes; we do NOT touch
    // reactions_left. Without a StartRound reset the second leave must not produce an AoO.
    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
    app.update();
    assert_eq!(aoo_events(&app).len(), 1, "first move triggers AoO");

    {
        use storyforge::combat::bridge::{entity_to_uid, CombatStateRes};
        use storyforge::combat_engine::PoolKind;
        let hero_uid = entity_to_uid(hero);
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(hero_uid).expect("hero in engine state");
        unit.pools[PoolKind::Mp] = Some((10, 10));
        unit.pos = start_pos();
        // DO NOT reset reactions_left — that's what the test is verifying.
    }
    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
    app.update();
    assert_eq!(
        aoo_events(&app).len(),
        1,
        "second move must not trigger — reaction spent"
    );
}

#[test]
fn stunned_enemy_no_opportunity() {
    let mut app = movement_app();
    insert_stun_status(&mut app);

    // Wave 3: engine rolls initiative in bootstrap_combat_state.
    // If goblin wins initiative and is first in turn order, settle_round_start
    // will skip-and-tick the stun (rounds_remaining 1→0) before the hero moves.
    // Use presets to ensure hero goes first so goblin's stun remains intact
    // when the AoO check fires.
    {
        use storyforge::game::resources::PresetInitiative;
        app.world_mut()
            .resource_mut::<PresetInitiative>()
            .0
            .insert("Hero".into(), 20);
        app.world_mut()
            .resource_mut::<PresetInitiative>()
            .0
            .insert("Goblin".into(), 5);
    }

    // With both units preset, roll_initiative_for_all draws no dice → script
    // is fully available for the AoO hit-roll (scripted to 8 to ensure a hit).
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[8]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut()
        .get_mut::<StatusEffects>(goblin)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "stun".into(),
            rounds_remaining: 1,
            // applier: None (environment/ability-applied, not hero-applied).
            // If applier=Some(hero), start_actor_turn(hero) calls tick_actor_statuses(hero)
            // which ticks and expires goblin's stun before the hero even moves — the stun
            // would be gone by the AoO check. Use applier=None to avoid that.
            applier: None,
            dot_per_tick: 0,
        });
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
    app.update();

    assert!(aoo_events(&app).is_empty(), "stunned enemy must not react");
}

/// Two provokers both adjacent; non-lethal hit → both fire (#3).
#[test]
fn multiple_provokers_all_fire() {
    let mut app = movement_app();
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[2, 2]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let g1 = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "G1");
    let g2 = spawn_at(
        &mut app,
        hex_from_offset(3, 4),
        test_enemy(base_stats()),
        "G2",
    );
    // Sanity: both flank hero, both leave adjacency after move to (2,3).
    assert_eq!(start_pos().unsigned_distance_to(hex_from_offset(3, 4)), 1);
    assert_eq!(away_pos().unsigned_distance_to(hex_from_offset(3, 4)), 2);
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
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
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[8, 8]);

    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let _g1 = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "G1");
    let _g2 = spawn_at(
        &mut app,
        hex_from_offset(3, 4),
        test_enemy(base_stats()),
        "G2",
    );
    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 1;
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let path = vec![away_pos(), hex_from_offset(1, 3), hex_from_offset(0, 3)];
    write_message(&mut app, ActionInput::Move { actor: hero, path });
    app.update();

    let events = aoo_events(&app);
    assert_eq!(
        events.len(),
        1,
        "second provoker must not fire after hero dies"
    );
    assert!(events[0].2, "killed flag set");
    // Dead units live in HexCorpses (not HexPositions) after projection.
    assert_eq!(
        app.world().resource::<HexCorpses>().get(&hero),
        Some(away_pos()),
        "path truncated at step 0 (died during first leave)"
    );
    assert_eq!(
        app.world().resource::<HexPositions>().get(&hero),
        None,
        "dead hero must not occupy HexPositions (occupancy layer)"
    );
    assert!(!app.world().get::<Vital>(hero).unwrap().is_alive());
    assert!(
        app.world().get::<Dead>(hero).is_some(),
        "Dead marker inserted"
    );
    let log = app.world().resource::<CombatLog>();
    assert!(
        log.0
            .iter()
            .any(|e| matches!(e, CombatEvent::UnitDied { entity } if *entity == hero)),
        "UnitDied event emitted for AoO kill"
    );
}

#[test]
fn no_melee_enemy_no_opportunity() {
    let mut app = movement_app();
    let hero = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut()
        .get_mut::<Abilities>(goblin)
        .unwrap()
        .0
        .clear();
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);
    init_engine_state(&mut app);

    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![away_pos()],
        },
    );
    app.update();

    assert!(
        aoo_events(&app).is_empty(),
        "enemy without melee must not react"
    );
}

/// Faction symmetry (#1): enemy moves, hero provokes.
#[test]
fn enemy_mover_hero_provokes() {
    let mut app = movement_app();
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[5]);

    let hero = spawn_at(&mut app, goblin_pos(), test_hero(base_stats()), "Hero");
    let goblin = spawn_at(&mut app, start_pos(), test_enemy(base_stats()), "Goblin");
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);
    init_engine_state(&mut app);

    let hp_before = app.world().get::<Vital>(goblin).unwrap().hp;
    write_message(
        &mut app,
        ActionInput::Move {
            actor: goblin,
            path: vec![away_pos()],
        },
    );
    app.update();

    let events = aoo_events(&app);
    assert_eq!(
        events.len(),
        1,
        "hero should AoO the fleeing goblin, got {events:?}"
    );
    assert_eq!(events[0].0, hero);
    assert!(app.world().get::<Vital>(goblin).unwrap().hp < hp_before);
}

// (Deleted in Phase 6 cleanup #4) `reactions_refill_on_round_start` tested
// the now-removed ECS-side `r.remaining = r.max` write in `build_turn_order`.
// The new contract: engine `CombatState::start_round` refills `reactions_left`
// via the `BumpRound` effect at end of EndTurn cascade, and projection writes
// the result back to ECS. Coverage of that path lives in:
//   - `crates/combat_engine/tests/turn_queue.rs::start_round_resets_reactions_for_alive_units_only`
//   - the bridge_* suites (end-to-end through EndTurn → projection)

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
    assert_eq!(
        step.unsigned_distance_to(goblin_pos()),
        1,
        "precondition: step still adjacent"
    );

    write_message(
        &mut app,
        ActionInput::Move {
            actor: hero,
            path: vec![step],
        },
    );
    app.update();

    assert!(aoo_events(&app).is_empty(), "stayed adjacent, no AoO");
    let r = app.world().get::<Reactions>(goblin).unwrap();
    assert_eq!(r.remaining, r.max);
}

/// Reproduction test for the original panic: AoO kills mover C on a hex already
/// occupied by ally A.
///
/// Setup:
///   C (hero, hp=1) at start_pos (3,3) — the mover; adjacent to B.
///   B (goblin)     at goblin_pos (4,3) — AoO source.
///   A (hero)       at away_pos   (2,3) — ally at C's first path step.
///
/// Path: C moves [away_pos, hex(1,3)]. The engine allows passthrough of ally
/// hex for non-final steps. AoO fires as C moves from start_pos (adjacent to B)
/// to away_pos (NOT adjacent to B). C dies; corpse lands at away_pos alongside A.
///
/// Before the `HexPositions`→`HexCorpses` split, the projector called
/// `HexPositions::insert(C_corpse, away_pos)` while A was already there → panic.
#[test]
fn aoo_kills_into_ally_hex_creates_corpse_at_shared_hex() {
    let mut app = movement_app();
    // Roll scripted to be lethal at hp=1, armor=0.
    app.world_mut()
        .resource_mut::<storyforge::combat::DiceRngRes>()
        .script(&[8]);

    let a = spawn_at(&mut app, away_pos(), test_hero(base_stats()), "A");
    let _b = spawn_at(&mut app, goblin_pos(), test_enemy(base_stats()), "B");
    let c = spawn_at(&mut app, start_pos(), test_hero(base_stats()), "C");
    app.world_mut().get_mut::<Vital>(c).unwrap().hp = 1;
    app.world_mut().entity_mut(c).insert(ActiveCombatant);
    init_engine_state(&mut app);

    // Path: [away_pos (A's hex), hex(1,3)]. away_pos is a non-final step so
    // the engine allows passthrough of A. AoO fires when C leaves start_pos
    // (adjacent to B) heading to away_pos (NOT adjacent to B). C dies.
    let path = vec![away_pos(), hex_from_offset(1, 3)];
    write_message(&mut app, ActionInput::Move { actor: c, path });
    app.update();

    let positions = app.world().resource::<HexPositions>();
    let corpses = app.world().resource::<HexCorpses>();

    // A must still be alive and in HexPositions at away_pos.
    assert_eq!(
        positions.entity_at(away_pos()),
        Some(a),
        "A must still occupy away_pos in HexPositions",
    );

    // C's corpse must be at away_pos alongside A.
    assert!(
        corpses.at(&away_pos()).contains(&c),
        "C's corpse must be in HexCorpses at away_pos",
    );

    // C must be dead and NOT in HexPositions.
    assert!(
        app.world().get::<Dead>(c).is_some(),
        "C must have Dead component",
    );
    assert_eq!(
        positions.get(&c),
        None,
        "C must not occupy HexPositions (dead units live in HexCorpses)",
    );
}
