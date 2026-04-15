mod common;

use bevy::prelude::*;

use common::*;
use storyforge::app_state::CombatPhase;
use storyforge::game::bundles::enemy_bundle;
use storyforge::game::components::{ActiveCombatant, CombatStats, Rage, Vital};
use storyforge::game::messages::{ApplyDamage, ApplyHeal, EndTurn};
use storyforge::game::resources::TurnQueue;

// ── Damage & armor ───────────────────────────────────────────────────────────

#[test]
fn apply_damage_reduces_hp() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: goblin,
            amount: 4,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(goblin).unwrap().hp, 6);
}

#[test]
fn armor_reduces_physical_damage() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((
            Name::new("Enemy"),
            enemy_bundle(base_stats(), 3, 3, vec![MELEE_ATTACK.into()], test_equipment()),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // 5 raw damage - 3 armor = 2 final damage.
    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: enemy,
            amount: 5,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(enemy).unwrap().hp, 8);
}

#[test]
fn spell_damage_pierces_armor() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((
            Name::new("Enemy"),
            enemy_bundle(base_stats(), 5, 3, vec![MELEE_ATTACK.into()], test_equipment()),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: enemy,
            amount: 4,
            breakdown: String::new(),
            pierces_armor: true,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(enemy).unwrap().hp, 6);
}

#[test]
fn minimum_damage_is_one() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((
            Name::new("Enemy"),
            enemy_bundle(base_stats(), 10, 3, vec![MELEE_ATTACK.into()], test_equipment()),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: enemy,
            amount: 2,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(enemy).unwrap().hp, 9);
}

// ── Healing ──────────────────────────────────────────────────────────────────

#[test]
fn heal_without_poison_restores_hp_normally() {
    let mut app = effects_app();

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyHeal {
            source: hero,
            target: hero,
            amount: 4,
            breakdown: String::new(),
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 9);
}

// ── Victory / Defeat ─────────────────────────────────────────────────────────

#[test]
fn killing_all_enemies_sets_victory_phase() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            test_enemy(CombatStats {
                max_hp: 1,
                ..base_stats()
            }),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: goblin,
            amount: 1,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();
    app.update(); // state transition frame

    assert_eq!(
        *app.world().resource::<State<CombatPhase>>(),
        CombatPhase::Victory
    );
}

#[test]
fn killing_all_heroes_sets_defeat_phase() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            test_hero(CombatStats {
                max_hp: 1,
                ..base_stats()
            }),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: goblin,
            target: hero,
            amount: 1,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: goblin });
    app.update();
    app.update();

    assert_eq!(
        *app.world().resource::<State<CombatPhase>>(),
        CombatPhase::Defeat
    );
}

// ── Rage ─────────────────────────────────────────────────────────────────────

#[test]
fn rage_gained_on_dealing_and_receiving_damage() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            test_hero(base_stats()),
            Rage::new(5),
        ))
        .id();
    let enemy = app
        .world_mut()
        .spawn((
            Name::new("Enemy"),
            test_enemy(base_stats()),
            Rage::new(5),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: enemy,
            amount: 3,
            breakdown: String::new(),
            pierces_armor: false,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Rage>(hero).unwrap().current, 1, "attacker gains rage");
    assert_eq!(app.world().get::<Rage>(enemy).unwrap().current, 1, "defender gains rage");
}
