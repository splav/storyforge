use storyforge::game::hex::hex_from_offset;
mod common;

use bevy::prelude::*;

use common::*;
use storyforge::core::DiceRng;
use storyforge::game::bundles::hero_bundle;
use storyforge::game::components::{
    ActiveCombatant, CombatPath, CombatStats, Mana, StatusEffects, Vital,
};
use storyforge::game::messages::ValidatedAction;
use storyforge::game::resources::{HexPositions, TurnQueue};

const BURN: &str = "burn";

fn mage_stats() -> CombatStats {
    CombatStats {
        max_hp: 20,
        strength: 0,
        dexterity: 5,
        constitution: 10,
        intelligence: 5,
        wisdom: 10,
        charisma: 10,
    }
}

fn spawn_mage(app: &mut App, path: &str) -> Entity {
    app.world_mut().spawn((
        Name::new("Mage"),
        hero_bundle(mage_stats(), 0, 3, vec![BURN.into()], test_equipment()),
        Mana::new(10),
        CombatPath(path.into()),
    )).id()
}

fn spawn_target(app: &mut App) -> Entity {
    app.world_mut().spawn((
        Name::new("Target"),
        test_enemy(base_stats()),
    )).id()
}

fn setup_turn(app: &mut App, actor: Entity, target: Entity) {
    let mut q = app.world_mut().resource_mut::<TurnQueue>();
    q.order = vec![actor, target];
    q.index = 0;
    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, hex_from_offset(0, 0));
    positions.insert(target, hex_from_offset(1, 0));
}

fn fire_burn(app: &mut App, actor: Entity, target: Entity) {
    write_message(app, ValidatedAction {
        actor,
        ability: BURN.into(),
        target,
        target_pos: hex_from_offset(1, 0),
        disadvantage: false,
    });
    app.update();
}

// ── Default Miss (no path) ───────────────────────────────────────────────────

#[test]
fn crit_fail_miss_no_effects_applied() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "will");
    app.world_mut().entity_mut(actor).remove::<CombatPath>();
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 9);
    let se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(se.0.is_empty(), "miss: no status on target");
}

#[test]
fn no_crit_fail_ability_fires_normally() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "will");
    app.world_mut().entity_mut(actor).remove::<CombatPath>();
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[10]);

    fire_burn(&mut app, actor, target);

    let se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(se.0.iter().any(|s| s.id.0 == "burning"), "normal roll: burning should be applied");
    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 9);
}

// ── Will: ManaOverload ───────────────────────────────────────────────────────

#[test]
fn crit_fail_will_mana_overload_doubles_cost() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "will");
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 8);
    let se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(se.0.iter().any(|s| s.id.0 == "burning"), "ManaOverload: ability should still fire");
}

#[test]
fn crit_fail_will_mana_overload_deficit_from_hp() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "will");
    app.world_mut().get_mut::<Mana>(actor).unwrap().current = 1;
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 0);
    assert_eq!(app.world().get::<Vital>(actor).unwrap().hp, 19, "deficit should come from HP");
}

// ── Faith: BrokenFaith ───────────────────────────────────────────────────────

#[test]
fn crit_fail_faith_applies_broken_faith_status() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "faith");
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    let target_se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(target_se.0.is_empty(), "faith crit fail: miss, no effect on target");
    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 9);
    let actor_se = app.world().get::<StatusEffects>(actor).unwrap();
    assert!(actor_se.0.iter().any(|s| s.id.0 == "broken_faith"),
        "faith crit fail: broken_faith status on actor");
}

// ── Tech: CircuitBreach ──────────────────────────────────────────────────────

#[test]
fn crit_fail_tech_self_damage() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "tech");
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 9);
    let hp = app.world().get::<Vital>(actor).unwrap().hp;
    assert!(hp < 20, "tech crit fail: actor should take self-damage, hp={hp}");
}

// ── Heritage: Exhaustion ─────────────────────────────────────────────────────

#[test]
fn crit_fail_heritage_applies_exhaustion_status() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "heritage");
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    let target_se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(target_se.0.is_empty(), "heritage crit fail: miss");
    let actor_se = app.world().get::<StatusEffects>(actor).unwrap();
    let exhaustion = actor_se.0.iter().find(|s| s.id.0 == "exhaustion");
    assert!(exhaustion.is_some(), "heritage crit fail: exhaustion on actor");
    assert_eq!(exhaustion.unwrap().rounds_remaining, 2);
}

// ── Pact: PactControl ────────────────────────────────────────────────────────

#[test]
fn crit_fail_pact_applies_pact_control_status() {
    let mut app = resolve_app();
    let actor = spawn_mage(&mut app, "pact");
    let target = spawn_target(&mut app);
    setup_turn(&mut app, actor, target);

    app.world_mut().resource_mut::<DiceRng>().script(&[1]);

    fire_burn(&mut app, actor, target);

    let target_se = app.world().get::<StatusEffects>(target).unwrap();
    assert!(target_se.0.is_empty(), "pact crit fail: miss");
    let actor_se = app.world().get::<StatusEffects>(actor).unwrap();
    let pact = actor_se.0.iter().find(|s| s.id.0 == "pact_control");
    assert!(pact.is_some(), "pact crit fail: pact_control on actor");
    assert_eq!(pact.unwrap().rounds_remaining, 1);
}
