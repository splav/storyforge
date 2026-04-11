use bevy::prelude::*;

mod app_state;
mod combat;
mod content;
mod core;
mod game;
mod ui;

use app_state::{AppState, CombatPhase};
use core::DiceRng;
use game::bundles::{enemy_bundle, warrior_bundle};
use game::components::{Mana, Rage};
use game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, StartCombat, UseAbility, ValidatedAction,
};
use game::resources::{CombatContext, CombatEvent, CombatLog, GameDb, SelectionState, TurnQueue};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Storyforge".into(),
                resolution: (900u32, 600u32).into(),
                ..default()
            }),
            ..default()
        }))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<DiceRng>()
        .init_resource::<ui::console_log::ConsoleCursor>()
        .add_message::<StartCombat>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_systems(Startup, (setup_demo, ui::combat_ui::setup_hud))
        .add_systems(
            Update,
            (
                ui::combat_ui::update_phase_hint,
                ui::combat_ui::update_turn_order,
                ui::combat_ui::update_combatants,
                ui::combat_ui::update_ability_panel,
                ui::log_ui::update_log,
                ui::console_log::print_log_system,
            )
                .run_if(in_state(AppState::Combat)),
        )
        .add_systems(
            Update,
            combat::start_combat_system.run_if(in_state(AppState::Overworld)),
        )
        .add_systems(
            Update,
            combat::turn_order::build_turn_order.run_if(in_state(CombatPhase::StartRound)),
        )
        .add_systems(
            Update,
            (
                combat::turn_start::turn_start_system,
                combat::skip_dead::skip_dead_turn_system,
                combat::skip_dead::skip_stunned_turn_system,
                combat::command_input::player_command_system,
                combat::enemy_ai::enemy_ai_system,
                combat::validation::validate_action_system,
                combat::resolution::resolve_action_system,
                combat::apply_effects::apply_effects_system,
                combat::advance_turn::advance_turn_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        )
        .run();
}

fn setup_demo(
    mut commands: Commands,
    db: Res<GameDb>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    commands.spawn(Camera2d);

    // Spawn players from class definitions.
    for (name, class_id) in [("Aldric", "warrior"), ("Lyra", "mage")] {
        let cls = db.classes.get(class_id).unwrap_or_else(|| {
            panic!(
                "Class '{class_id}' not found in {}",
                "assets/data/classes.toml"
            )
        });
        let mut ec = commands.spawn((
            Name::new(name),
            warrior_bundle(cls.stats.clone(), cls.abilities.clone(), cls.weapon.clone()),
        ));
        if cls.rage_max > 0 {
            ec.insert(Rage::new(cls.rage_max));
        }
        if cls.mana_max > 0 {
            ec.insert(Mana::new(cls.mana_max));
        }
    }

    // Spawn enemies from the first encounter in the database.
    let enc = db.encounters.get("goblin_patrol").unwrap_or_else(|| {
        panic!("Encounter 'goblin_patrol' not found in assets/data/encounters.toml")
    });

    for enemy in &enc.enemies {
        commands.spawn((
            Name::new(enemy.name.clone()),
            enemy_bundle(
                enemy.stats.clone(),
                enemy.ability_ids.clone(),
                enemy.weapon_id.clone(),
            ),
        ));
    }

    let encounter_entity = commands.spawn(Name::new(enc.name.clone())).id();
    ctx.encounter = Some(encounter_entity);
    log.push(CombatEvent::CombatStarted);
    next_state.set(AppState::Combat);
}
