/// Integration tests for the combat pipeline.
use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::{
    advance_turn::advance_turn_system, apply_effects::apply_effects_system,
    enemy_ai::enemy_ai_system, skip_dead::skip_stunned_turn_system,
    validation::validate_action_system,
};
const MELEE_ATTACK: &str = "melee_attack";
const SHORT_SWORD: &str = "short_sword";
use storyforge::core::DiceRng;
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::components::{ActionPoints, ActiveStatus, CombatStats, StatusEffects, Vital};
use storyforge::game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, MoveUnit, UseAbility, ValidatedAction,
};
use storyforge::content::statuses::StatusDef;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{
    CombatContext, GameDb, HexPositions, SelectionState, TurnQueue,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 10,
        armor: 0,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
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
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let target = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();

    app.world_mut().resource_mut::<CombatContext>().active = Some(actor);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
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
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let other = app
        .world_mut()
        .spawn((
            Name::new("Hero2"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let target = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();

    app.world_mut().resource_mut::<CombatContext>().active = Some(other);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
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
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let target = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();

    app.world_mut()
        .get_mut::<storyforge::game::components::ActionPoints>(actor)
        .unwrap()
        .action = false;

    app.world_mut().resource_mut::<CombatContext>().active = Some(actor);
    write_message(
        &mut app,
        UseAbility {
            actor,
            ability: MELEE_ATTACK.into(),
            target,
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
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(hero);

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
        .spawn((
            Name::new("Hero"),
            hero_bundle(
                CombatStats {
                    max_hp: 10,
                    armor: 0,
                    strength: 5,
                    dexterity: 5,
                    constitution: 10,
                    intelligence: 0,
                    wisdom: 10,
                    charisma: 10,
                },
                3,
                vec![MELEE_ATTACK.into()],
                SHORT_SWORD.into(),
            ),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(
                CombatStats {
                    max_hp: 1,
                    armor: 0,
                    strength: 3,
                    dexterity: 5,
                    constitution: 10,
                    intelligence: 0,
                    wisdom: 10,
                    charisma: 10,
                },
                3,
                vec![MELEE_ATTACK.into()],
                SHORT_SWORD.into(),
            ),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(hero);

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
            hero_bundle(
                CombatStats {
                    max_hp: 1,
                    armor: 0,
                    strength: 5,
                    dexterity: 5,
                    constitution: 10,
                    intelligence: 0,
                    wisdom: 10,
                    charisma: 10,
                },
                3,
                vec![MELEE_ATTACK.into()],
                SHORT_SWORD.into(),
            ),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(
                CombatStats {
                    max_hp: 10,
                    armor: 0,
                    strength: 3,
                    dexterity: 5,
                    constitution: 10,
                    intelligence: 0,
                    wisdom: 10,
                    charisma: 10,
                },
                3,
                vec![MELEE_ATTACK.into()],
                SHORT_SWORD.into(),
            ),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(goblin);

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
fn turn_ending_flag_cleared_on_advance() {
    let mut app = effects_app();
    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, goblin];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(hero);
    app.world_mut().resource_mut::<CombatContext>().turn_ending = true;

    write_message(&mut app, EndTurn { actor: hero });
    app.update();

    let ctx = app.world().resource::<CombatContext>();
    assert_eq!(ctx.active, Some(goblin));
    assert!(!ctx.turn_ending, "turn_ending should be cleared for the new active actor");
}

#[test]
fn stunned_unit_skips_turn_and_stun_expires_on_applier_end_turn() {
    let mut app = stun_app();
    insert_stun_status(&mut app);

    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
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
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, hero];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(goblin);

    // Frame 1: goblin's turn — skip_stunned sends EndTurn, advance to hero.
    app.update();

    let ctx = app.world().resource::<CombatContext>();
    assert_eq!(ctx.active, Some(hero), "stunned goblin should be skipped, hero is next");

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
        .spawn((
            Name::new("Hero"),
            hero_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let goblin = app
        .world_mut()
        .spawn((
            Name::new("Goblin"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
        .id();
    let goblin2 = app
        .world_mut()
        .spawn((
            Name::new("Goblin2"),
            enemy_bundle(base_stats(), 3, vec![MELEE_ATTACK.into()], SHORT_SWORD.into()),
        ))
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
        });

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![goblin, goblin2, hero];
        q.index = 0;
    }
    app.world_mut().resource_mut::<CombatContext>().active = Some(goblin);

    // Goblin is stunned → skip_stunned sends EndTurn.
    // enemy_ai would also send EndTurn (ap=0) but advance_turn deduplicates.
    // Should advance to goblin2, NOT skip goblin2.
    app.update();

    let ctx = app.world().resource::<CombatContext>();
    assert_eq!(
        ctx.active,
        Some(goblin2),
        "stunned goblin's turn should advance to goblin2, not skip it"
    );
    let queue = app.world().resource::<TurnQueue>();
    assert_eq!(queue.index, 1);
}
