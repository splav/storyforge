/// Integration tests for the combat pipeline.
use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::{
    advance_turn::advance_turn_system, apply_effects::apply_effects_system,
    validation::validate_action_system,
};
const MELEE_ATTACK: &str = "melee_attack";
const SHORT_SWORD: &str = "short_sword";
use storyforge::core::DiceRng;
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::components::{CombatStats, Vital};
use storyforge::game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, UseAbility, ValidatedAction,
};
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
