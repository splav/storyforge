use bevy::prelude::*;

use crate::common::*;
use storyforge::app_state::CombatPhase;
use storyforge::game::bundles::enemy_bundle;
use storyforge::core::DiceRng;
use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
use storyforge::game::components::{ActiveCombatant, ActiveStatus, CombatStats, Dead, EnemyPhases, Energy, Mana, Rage, StatusEffects, Vital};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::{ApplyDamage, ApplyHeal, EndTurn, ValidatedAction};
use storyforge::game::resources::{HexPositions, TurnQueue};

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

// ── Rest: restores HP and all resources by 1 ─────────────────────────────────

#[test]
fn rest_restores_hp_mana_rage_energy() {
    let mut app = resolve_app();
    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            test_hero(base_stats()),
            Mana::new(5),
            Rage::new(5),
            Energy::new(5),
        ))
        .id();
    let dummy = app
        .world_mut()
        .spawn((Name::new("Dummy"), test_enemy(base_stats())))
        .id();

    // Drain HP and resources so restore has room to act.
    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;
    app.world_mut().get_mut::<Mana>(hero).unwrap().current = 2;
    app.world_mut().get_mut::<Energy>(hero).unwrap().current = 2;
    // Rage starts at 0 via Rage::new.

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, dummy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(hero, hex_from_offset(0, 0));
    positions.insert(dummy, hex_from_offset(5, 0));

    // d20=10 → no crit fail.
    app.world_mut().resource_mut::<DiceRng>().script(&[10]);

    write_message(
        &mut app,
        ValidatedAction {
            actor: hero,
            ability: "rest".into(),
            target: hero,
            target_pos: hex_from_offset(0, 0),
            disadvantage: false,
        },
    );
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 6, "rest heals 1 hp");
    assert_eq!(app.world().get::<Mana>(hero).unwrap().current, 3, "rest +1 mana");
    assert_eq!(app.world().get::<Rage>(hero).unwrap().current, 1, "rest +1 rage");
    assert_eq!(app.world().get::<Energy>(hero).unwrap().current, 3, "rest +1 energy");
}

#[test]
fn rest_does_not_exceed_maximums() {
    let mut app = resolve_app();
    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            test_hero(base_stats()),
            Mana::new(5),
            Rage::new(5),
            Energy::new(5),
        ))
        .id();
    let dummy = app
        .world_mut()
        .spawn((Name::new("Dummy"), test_enemy(base_stats())))
        .id();

    // All at max.
    app.world_mut().get_mut::<Rage>(hero).unwrap().current = 5;

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, dummy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(hero, hex_from_offset(0, 0));
    positions.insert(dummy, hex_from_offset(5, 0));

    app.world_mut().resource_mut::<DiceRng>().script(&[10]);

    write_message(
        &mut app,
        ValidatedAction {
            actor: hero,
            ability: "rest".into(),
            target: hero,
            target_pos: hex_from_offset(0, 0),
            disadvantage: false,
        },
    );
    app.update();

    let max_hp = app.world().get::<Vital>(hero).unwrap().max_hp;
    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, max_hp, "hp clamps at max");
    assert_eq!(app.world().get::<Mana>(hero).unwrap().current, 5, "mana clamps at max");
    assert_eq!(app.world().get::<Rage>(hero).unwrap().current, 5, "rage clamps at max");
    assert_eq!(app.world().get::<Energy>(hero).unwrap().current, 5, "energy clamps at max");
}

// ── Phase transitions ────────────────────────────────────────────────────────

#[test]
fn lethal_damage_triggers_phase_revive_and_renames() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();

    // Boss with 10 HP and a single phase that fires on death:
    // renames to "Эхо", heals to full, and bumps max_hp to 20.
    let boss_stats = CombatStats { max_hp: 10, ..base_stats() };
    let phased_stats = CombatStats { max_hp: 20, ..base_stats() };
    let boss = app
        .world_mut()
        .spawn((
            Name::new("Босс"),
            test_enemy(boss_stats),
            EnemyPhases {
                pending: vec![PhaseDef {
                    trigger: PhaseTrigger::HpBelowPct(1),
                    name: Some("Эхо".into()),
                    stats: Some(phased_stats),
                    ability_ids: None,
                    heal_to_full: true,
                    flavor: None,
                }],
            },
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, boss];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // Overkill damage — apply_effects marks Dead; phase system must revive before advance_turn.
    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: boss,
            amount: 50,
            breakdown: String::new(),
            pierces_armor: true,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let vital = app.world().get::<Vital>(boss).unwrap();
    assert_eq!(vital.hp, 20, "phase heals to new max_hp");
    assert_eq!(vital.max_hp, 20, "max_hp replaced by phase stats");
    assert!(app.world().get::<Dead>(boss).is_none(), "Dead should be cleared on phase entry");
    assert_eq!(app.world().get::<Name>(boss).unwrap().as_str(), "Эхо");
    let phases = app.world().get::<EnemyPhases>(boss).unwrap();
    assert!(phases.pending.is_empty(), "phase consumed after firing");
}

#[test]
fn dot_tick_lethal_damage_still_triggers_phase_revive() {
    // Regression: босса с KillTarget-фазой добивало DoT-тиком, и бой
    // схлопывался в Victory до фазового перехода. После фикса DoT-тик
    // переехал в `tick_status_effects_system` на `TurnStart` повесившего —
    // падение HP случается в начале того же кадра, в `Execute` того же
    // кадра `phase_transition_system` успевает оживить босса до
    // victory-check в `advance_turn_system` (Finalize).
    let mut app = effects_app();
    insert_poison_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();

    let boss_stats = CombatStats { max_hp: 10, ..base_stats() };
    let phased_stats = CombatStats { max_hp: 20, ..base_stats() };
    let boss = app
        .world_mut()
        .spawn((
            Name::new("Босс"),
            test_enemy(boss_stats),
            EnemyPhases {
                pending: vec![PhaseDef {
                    trigger: PhaseTrigger::HpBelowPct(1),
                    name: Some("Эхо".into()),
                    stats: Some(phased_stats),
                    ability_ids: None,
                    heal_to_full: true,
                    flavor: None,
                }],
            },
        ))
        .id();

    // Сильный яд от героя — добьёт босса одним тиком.
    app.world_mut()
        .get_mut::<StatusEffects>(boss)
        .unwrap()
        .0
        .push(ActiveStatus {
            id: "poisoned".into(),
            rounds_remaining: 2,
            applier: hero,
            dot_per_tick: 50,
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, boss];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // Первый update: новый ActiveCombatant=hero → `tick_status_effects`
    // в TurnStart добивает босса ядом, `phase_transition_system` в Execute
    // того же кадра оживляет по HpBelowPct, `advance_turn_system` в Finalize
    // видит уже живого босса → Victory не срабатывает.
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_ne!(
        *app.world().resource::<State<CombatPhase>>().get(),
        CombatPhase::Victory,
        "combat must not end — phase should revive DoT-killed boss",
    );
    let vital = app.world().get::<Vital>(boss).unwrap();
    assert_eq!(vital.hp, 20, "phase heals to new max_hp after DoT kill");
    assert_eq!(vital.max_hp, 20);
    assert!(app.world().get::<Dead>(boss).is_none(), "Dead cleared on phase entry");
    assert_eq!(app.world().get::<Name>(boss).unwrap().as_str(), "Эхо");
    let phases = app.world().get::<EnemyPhases>(boss).unwrap();
    assert!(phases.pending.is_empty(), "phase consumed");
}

#[test]
fn phase_does_not_fire_when_trigger_not_met() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();

    let boss_stats = CombatStats { max_hp: 10, ..base_stats() };
    let boss = app
        .world_mut()
        .spawn((
            Name::new("Босс"),
            test_enemy(boss_stats),
            EnemyPhases {
                pending: vec![PhaseDef {
                    trigger: PhaseTrigger::HpBelowPct(50),
                    name: Some("Эхо".into()),
                    stats: None,
                    ability_ids: None,
                    heal_to_full: false,
                    flavor: None,
                }],
            },
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, boss];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    // 4 damage out of 10 → 60% hp remaining → above 50% threshold.
    write_message(
        &mut app,
        ApplyDamage {
            source: hero,
            target: boss,
            amount: 4,
            breakdown: String::new(),
            pierces_armor: true,
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Name>(boss).unwrap().as_str(), "Босс");
    let phases = app.world().get::<EnemyPhases>(boss).unwrap();
    assert_eq!(phases.pending.len(), 1, "phase must remain pending");
}
