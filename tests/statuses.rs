mod common;

use bevy::prelude::*;

use common::*;
use storyforge::app_state::CombatPhase;
use storyforge::game::components::{
    ActiveCombatant, ActiveStatus, CombatStats, Dead, StatusEffects, Vital,
};
use storyforge::game::messages::{ApplyHeal, ApplyStatus, EndTurn};
use storyforge::game::resources::TurnQueue;

// ── EndTurn dedup ────────────────────────────────────────────────────────────

#[test]
fn duplicate_end_turn_does_not_double_advance() {
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

    write_message(&mut app, EndTurn { actor: hero });
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert!(app.world().get::<ActiveCombatant>(goblin).is_some(), "goblin should be active");
    assert!(app.world().get::<ActiveCombatant>(hero).is_none(), "hero should not be active");
}

// ── Stun ─────────────────────────────────────────────────────────────────────

#[test]
fn stunned_unit_skips_turn_and_stun_expires_on_applier_end_turn() {
    let mut app = stun_app();
    insert_stun_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

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

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    app.update();

    assert!(app.world().get::<ActiveCombatant>(hero).is_some(), "stunned goblin should be skipped");

    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert_eq!(se.0.len(), 1, "stun should still be active after goblin's skipped turn");
    assert_eq!(se.0[0].rounds_remaining, 1);

    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert!(se.0.is_empty(), "stun should have expired after hero's EndTurn");
}

#[test]
fn stunned_enemy_does_not_produce_duplicate_end_turn() {
    let mut app = stun_app();
    insert_stun_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();
    let goblin2 = app
        .world_mut()
        .spawn((Name::new("Goblin2"), test_enemy(base_stats())))
        .id();

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

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, goblin2, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    app.update();

    assert!(
        app.world().get::<ActiveCombatant>(goblin2).is_some(),
        "stunned goblin's turn should advance to goblin2, not skip it"
    );
    let queue = app.world().resource::<TurnQueue>();
    assert_eq!(queue.index, 1);
}

// ── Dead units in turn queue ─────────────────────────────────────────────────

#[test]
fn dead_applier_status_still_expires() {
    let mut app = effects_app();
    insert_stun_status(&mut app);

    let alive_hero = app
        .world_mut()
        .spawn((Name::new("AliveHero"), test_hero(base_stats())))
        .id();
    let dead_hero = app
        .world_mut()
        .spawn((Name::new("DeadHero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<StatusEffects>(enemy)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "stun".into(),
            rounds_remaining: 1,
            applier: dead_hero,
            dot_per_tick: 0,
        });

    app.world_mut().entity_mut(dead_hero).insert(Dead);
    app.world_mut().get_mut::<Vital>(dead_hero).unwrap().hp = 0;

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![enemy, dead_hero, alive_hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(enemy).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: enemy });
    app.update();

    let se = app.world().get::<StatusEffects>(enemy).unwrap();
    assert!(se.0.is_empty(), "stun should expire after dead hero's virtual turn ticks it");
}

#[test]
fn dead_unit_skipped_at_queue_start() {
    let mut app = effects_app();

    let dead_hero = app
        .world_mut()
        .spawn((Name::new("DeadHero"), test_hero(base_stats())))
        .id();
    let alive_hero = app
        .world_mut()
        .spawn((Name::new("AliveHero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(dead_hero).insert(Dead);
    app.world_mut().get_mut::<Vital>(dead_hero).unwrap().hp = 0;

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![dead_hero, enemy, alive_hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(dead_hero).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: dead_hero });
    app.update();

    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_some(),
        "dead hero at queue start should be skipped"
    );
}

// ── Poison / DoT ─────────────────────────────────────────────────────────────

#[test]
fn poison_ticks_damage_each_turn() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 2,
            applier: goblin,
            dot_per_tick: 3,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: goblin });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 7);
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert_eq!(se.0.len(), 1);
    assert_eq!(se.0[0].rounds_remaining, 1);
}

#[test]
fn poison_expires_after_last_tick() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 1,
            applier: goblin,
            dot_per_tick: 2,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: goblin });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 8);
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert!(se.0.is_empty(), "poison should expire after last tick");
}

#[test]
fn poison_can_kill() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            test_hero(CombatStats { max_hp: 2, ..base_stats() }),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 3,
            applier: goblin,
            dot_per_tick: 5,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(goblin).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: goblin });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 0);
    assert!(app.world().get::<Dead>(hero).is_some(), "poison should be able to kill");
}

// ── Burn duration ───────────────────────────────────────────────────────────

#[test]
fn burning_lasts_two_applier_end_turns() {
    let mut app = effects_app();
    insert_burning_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    // Hero applies burning with duration 2 (matching abilities.toml).
    app.world_mut()
        .get_mut::<StatusEffects>(goblin)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "burning".into(),
            rounds_remaining: 2,
            applier: hero,
            dot_per_tick: 0,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // Turn 1: hero ends turn → burning ticks from 2 to 1.
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert_eq!(se.0.len(), 1, "burning should still be active after first hero EndTurn");
    assert_eq!(se.0[0].rounds_remaining, 1);

    // Goblin's turn — burning should NOT tick (goblin is not the applier).
    write_message(&mut app, EndTurn { actor: goblin });
    app.update();

    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert_eq!(se.0.len(), 1, "burning should not tick on goblin's own EndTurn");
    assert_eq!(se.0[0].rounds_remaining, 1);

    // Round boundary: goblin's EndTurn wraps the queue → StartRound.
    // Re-enter AwaitCommand for the next round.
    enter_await_command(&mut app);
    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // Turn 2: hero ends turn again → burning ticks from 1 to 0 → expires.
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert!(se.0.is_empty(), "burning should expire after second hero EndTurn");

    // Goblin took no DoT damage — burning is purely a vulnerability modifier.
    assert_eq!(app.world().get::<Vital>(goblin).unwrap().hp, 10);
}

// ── Heal + Poison interaction ────────────────────────────────────────────────

#[test]
fn heal_neutralizes_poison_fully() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 3,
            applier: goblin,
            dot_per_tick: 3,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyHeal { source: hero, target: hero, amount: 5, breakdown: String::new() },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 7);
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert!(se.0.is_empty(), "poison should be fully cleansed");
}

#[test]
fn heal_weakens_poison_partially() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 3,
            applier: goblin,
            dot_per_tick: 3,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyHeal { source: hero, target: hero, amount: 2, breakdown: String::new() },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 5, "no HP restored — heal spent on poison");
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert_eq!(se.0.len(), 1, "poison still active but weakened");
    assert_eq!(se.0[0].dot_per_tick, 1, "dot_per_tick should be reduced from 3 to 1");
}

#[test]
fn heal_exact_match_removes_poison_no_hp() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let goblin = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;

    app.world_mut()
        .get_mut::<StatusEffects>(hero)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 2,
            applier: goblin,
            dot_per_tick: 4,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyHeal { source: hero, target: hero, amount: 4, breakdown: String::new() },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 5, "heal exactly matched poison");
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert!(se.0.is_empty(), "poison should be removed by exact heal");
}

// ── Status refresh ───────────────────────────────────────────────────────────

#[test]
fn reapplying_status_replaces_previous() {
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<StatusEffects>(enemy)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 3,
            applier: hero,
            dot_per_tick: 5,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(
        &mut app,
        ApplyStatus {
            source: hero,
            target: enemy,
            status: "poisoned".into(),
            duration_rounds: 1,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let se = app.world().get::<StatusEffects>(enemy).unwrap();
    let poisons: Vec<_> = se.0.iter().filter(|s| s.id.0 == "poisoned").collect();
    assert_eq!(poisons.len(), 1, "reapply should replace, not stack");
}

// ── DoT during advance kills last enemy → victory ───────────────────────────

#[test]
fn dot_during_advance_loop_triggers_victory() {
    // Setup: dead_hero applied poison on enemy (1 HP). Hero ends turn.
    // Advance loop skips dead_hero, ticks poison → enemy dies → victory.
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let dead_hero = app
        .world_mut()
        .spawn((Name::new("DeadHero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((
            Name::new("Enemy"),
            test_enemy(CombatStats { max_hp: 1, ..base_stats() }),
        ))
        .id();

    // dead_hero is dead.
    app.world_mut().entity_mut(dead_hero).insert(Dead);
    app.world_mut().get_mut::<Vital>(dead_hero).unwrap().hp = 0;

    // Poison on enemy, applied by dead_hero, dot_per_tick = 3.
    app.world_mut()
        .get_mut::<StatusEffects>(enemy)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 2,
            applier: dead_hero,
            dot_per_tick: 3,
        });

    // Queue: [hero, dead_hero, enemy]. Hero ends turn → advance skips dead_hero
    // (ticks poison on enemy → enemy dies) → advance finds no living enemies.
    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, dead_hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    write_message(&mut app, EndTurn { actor: hero });
    app.update();
    app.update(); // state transition frame

    assert!(app.world().get::<Dead>(enemy).is_some(), "enemy should be dead from DoT");
    assert_eq!(
        *app.world().resource::<State<CombatPhase>>(),
        CombatPhase::Victory,
        "DoT killing last enemy during advance should trigger victory"
    );
}
