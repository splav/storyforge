/// Integration tests for the combat pipeline.
use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::{
    advance_turn::advance_turn_system, ai_difficulty::DifficultyProfile,
    apply_effects::apply_effects_system, enemy_ai::enemy_ai_system,
    skip_dead::{skip_dead_turn_system, skip_stunned_turn_system},
    validation::validate_action_system,
};
const MELEE_ATTACK: &str = "melee_attack";
use storyforge::core::DiceRng;
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::components::{ActiveCombatant, ActiveStatus, CombatStats, Dead, Equipment, StatusEffects, Vital};
use storyforge::game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, MoveUnit, UseAbility, ValidatedAction,
};
use storyforge::content::statuses::StatusDef;
use storyforge::core::DiceExpr;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{
    CombatContext, GameDb, HexPositions, SelectionState, TurnQueue,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 10,
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

fn test_hero(stats: CombatStats) -> impl Bundle {
    hero_bundle(stats, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
}

fn test_enemy(stats: CombatStats) -> impl Bundle {
    enemy_bundle(stats, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
}

fn validation_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<DiceRng>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_systems(
            Update,
            validate_action_system.run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

fn effects_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<DiceRng>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_systems(
            Update,
            (apply_effects_system, advance_turn_system)
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

fn enter_await_command(app: &mut App) {
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Combat);
    app.update();
    app.world_mut()
        .resource_mut::<NextState<CombatPhase>>()
        .set(CombatPhase::AwaitCommand);
    app.update();
}

fn write_message<M: Message>(app: &mut App, msg: M) {
    app.world_mut().resource_mut::<Messages<M>>().write(msg);
}

fn message_count<M: Message>(app: &App) -> usize {
    app.world()
        .resource::<Messages<M>>()
        .iter_current_update_messages()
        .count()
}

// ── validation ────────────────────────────────────────────────────────────────

#[test]
fn valid_use_ability_emits_validated_action() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: (0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 1);
}

#[test]
fn wrong_actor_use_ability_is_rejected() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let other = app
        .world_mut()
        .spawn((Name::new("Hero2"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut().entity_mut(other).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: (0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

#[test]
fn no_action_point_use_ability_is_rejected() {
    let mut app = validation_app();
    let actor = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let target = app
        .world_mut()
        .spawn((Name::new("Goblin"), test_enemy(base_stats())))
        .id();

    app.world_mut()
        .get_mut::<storyforge::game::components::ActionPoints>(actor)
        .unwrap()
        .action = false;

    app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
            target_pos: (0, 0),
        },
    );
    app.update();

    assert_eq!(message_count::<ValidatedAction>(&app), 0);
}

// ── cleanup ───────────────────────────────────────────────────────────────────

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

    // armor=0 so full 4 damage applied
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
                strength: 3,
                dexterity: 5,
                constitution: 10,
                intelligence: 0,
                wisdom: 10,
                charisma: 10,
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
                strength: 5,
                dexterity: 5,
                constitution: 10,
                intelligence: 0,
                wisdom: 10,
                charisma: 10,
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

// ── EndTurn dedup + stun ─────────────────────────────────────────────────────

/// Build an app with skip_stunned → apply_effects → advance_turn chained.
fn insert_stun_status(app: &mut App) {
    app.world_mut().resource_mut::<GameDb>().statuses.insert(
        "stun".into(),
        StatusDef {
            id: "stun".into(),
            name: "Stun".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
        },
    );
}

fn stun_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<DiceRng>()
        .init_resource::<DifficultyProfile>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_message::<UseAbility>()
        .add_message::<MoveUnit>()
        .add_systems(
            Update,
            (
                skip_stunned_turn_system,
                enemy_ai_system,
                apply_effects_system,
                advance_turn_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

#[test]
fn duplicate_end_turn_does_not_double_advance() {
    // Two EndTurn messages for the same actor in one frame must not advance twice.
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

    // Send EndTurn twice for the same actor.
    write_message(&mut app, EndTurn { actor: hero });
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    // Goblin should be active — not wrapped back to hero.
    assert!(app.world().get::<ActiveCombatant>(goblin).is_some(), "goblin should be active");
    assert!(app.world().get::<ActiveCombatant>(hero).is_none(), "hero should not be active");
}

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

    // Apply stun on goblin, applied by hero, duration 1.
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

    // Frame 1: goblin's turn — skip_stunned sends EndTurn, advance to hero.
    app.update();

    assert!(app.world().get::<ActiveCombatant>(hero).is_some(), "stunned goblin should be skipped, hero is next");

    // Goblin should still have stun (ticks on HERO's EndTurn, not goblin's).
    let se = app.world().get::<StatusEffects>(goblin).unwrap();
    assert_eq!(se.0.len(), 1, "stun should still be active after goblin's skipped turn");
    assert_eq!(se.0[0].rounds_remaining, 1);

    // Frame 2: hero's turn — send EndTurn for hero. Stun ticks (applier=hero).
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

    // Stun goblin (applied by hero).
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

    // Goblin is stunned → skip_stunned sends EndTurn.
    // enemy_ai would also send EndTurn (ap=0) but advance_turn deduplicates.
    // Should advance to goblin2, NOT skip goblin2.
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
    // Dead hero applied stun on enemy. When advancing past the dead hero,
    // the stun must tick down and expire.
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

    // Stun on enemy, applied by dead_hero, duration 1.
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

    // dead_hero is dead.
    app.world_mut().entity_mut(dead_hero).insert(Dead);
    app.world_mut().get_mut::<Vital>(dead_hero).unwrap().hp = 0;

    // Queue: [enemy, dead_hero, alive_hero].
    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![enemy, dead_hero, alive_hero];
        q.index = 0;
    }
    app.world_mut().entity_mut(enemy).insert(ActiveCombatant);

    // Enemy ends turn → advance skips dead_hero (ticks dead_hero's statuses → stun expires)
    // → advances to alive_hero.
    write_message(&mut app, EndTurn { actor: enemy });
    app.update();

    let se = app.world().get::<StatusEffects>(enemy).unwrap();
    assert!(se.0.is_empty(), "stun should expire after dead hero's virtual turn ticks it");
}

#[test]
fn dead_unit_skipped_at_queue_start() {
    // Queue starts with a dead unit — it should be skipped,
    // the next living unit becomes active.
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
        "dead hero at queue start should be skipped, enemy becomes active"
    );
    assert!(
        app.world().get::<ActiveCombatant>(dead_hero).is_none(),
        "dead hero should not remain active"
    );
}

// ── Poison / DoT ────────────────────────────────────────────────────────────

fn insert_poison_status(app: &mut App) {
    app.world_mut().resource_mut::<GameDb>().statuses.insert(
        "poisoned".into(),
        StatusDef {
            id: "poisoned".into(),
            name: "Poisoned".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: false,
            forces_targeting: false,
            dot_dice: Some(DiceExpr::new(1, 4, 0)),
        },
    );
}

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

    // Poison on hero, applied by goblin, duration 2, dot_per_tick = 3.
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

    // Goblin ends turn → poison ticks on hero (applier = goblin).
    write_message(&mut app, EndTurn { actor: goblin });
    app.update();

    // Hero lost 3 HP from poison tick 1.
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

    // Duration 1 → should tick once and expire.
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
            test_hero(CombatStats {
                max_hp: 2,
                strength: 5,
                dexterity: 5,
                constitution: 10,
                intelligence: 0,
                wisdom: 10,
                charisma: 10,
            }),
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

// ── Heal + Poison interaction ───────────────────────────────────────────────

#[test]
fn heal_neutralizes_poison_fully() {
    // Heal (5) > dot_per_tick (3) → poison removed, remaining 2 heals HP.
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

    // Damage hero first so heal has something to restore.
    app.world_mut().get_mut::<Vital>(hero).unwrap().hp = 5;

    // Poison with dot_per_tick = 3.
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

    // Heal hero for 5: 3 neutralizes poison, 2 heals HP (5→7).
    write_message(
        &mut app,
        ApplyHeal {
            source: hero,
            target: hero,
            amount: 5,
            breakdown: String::new(),
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 7);
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert!(se.0.is_empty(), "poison should be fully cleansed");
}

#[test]
fn heal_weakens_poison_partially() {
    // Heal (2) < dot_per_tick (3) → poison weakened to 1, no HP restored.
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

    // Heal hero for 2: all goes to weakening poison (3→1), HP stays 5.
    write_message(
        &mut app,
        ApplyHeal {
            source: hero,
            target: hero,
            amount: 2,
            breakdown: String::new(),
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 5, "no HP restored — heal spent on poison");
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert_eq!(se.0.len(), 1, "poison still active but weakened");
    assert_eq!(se.0[0].dot_per_tick, 1, "dot_per_tick should be reduced from 3 to 1");
}

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

#[test]
fn heal_exact_match_removes_poison_no_hp() {
    // Heal exactly equals dot_per_tick → poison removed, 0 HP restored.
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
        ApplyHeal {
            source: hero,
            target: hero,
            amount: 4,
            breakdown: String::new(),
        },
    );
    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    assert_eq!(app.world().get::<Vital>(hero).unwrap().hp, 5, "heal exactly matched poison — no HP change");
    let se = app.world().get::<StatusEffects>(hero).unwrap();
    assert!(se.0.is_empty(), "poison should be removed by exact heal");
}
