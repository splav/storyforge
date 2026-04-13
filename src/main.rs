use bevy::prelude::*;

mod app_state;
mod combat;
mod content;
mod core;
mod game;
mod scenario;
mod ui;

use app_state::{AppState, CombatPhase};
use core::DiceRng;
use game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, MoveUnit, StartCombat, UseAbility,
    ValidatedAction,
};
use game::combat_log::CombatLog;
use game::resources::{CombatContext, GameDb, HexPositions, SelectionState, TurnQueue};
use ui::animation::AnimationQueue;

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
        .init_resource::<HexPositions>()
        .init_resource::<ui::hex_grid::HexHover>()
        .init_resource::<ui::hex_grid::HexLastClick>()
        .init_resource::<AnimationQueue>()
        .init_resource::<combat::enemy_popup::PopupCursor>()
        .add_message::<StartCombat>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<MoveUnit>()
        .add_message::<EndTurn>()
        .add_message::<scenario::AdvanceScenario>()
        .add_systems(
            Startup,
            (
                scenario::start_scenario,
                ui::combat_ui::setup_hud,
                ui::hex_grid::setup_hex_grid,
            ),
        )
        // ── Story ────────────────────────────────────────────────────────
        .add_systems(OnEnter(AppState::Story), ui::story_ui::setup_story_screen)
        .add_systems(
            Update,
            ui::story_ui::story_input_system.run_if(in_state(AppState::Story)),
        )
        .add_systems(
            OnExit(AppState::Story),
            ui::story_ui::cleanup_story_screen,
        )
        // ── Combat enter / exit ──────────────────────────────────────────
        .add_systems(
            OnEnter(AppState::Combat),
            (
                scenario::combat_scene::spawn_combat_scene,
                ui::hex_grid::assign_hex_positions,
            )
                .chain(),
        )
        .add_systems(
            OnExit(AppState::Combat),
            scenario::combat_scene::despawn_combatants,
        )
        // ── Combat UI (runs every frame during combat) ───────────────────
        .add_systems(
            Update,
            (
                ui::combat_ui::update_phase_hint,
                ui::combat_ui::update_turn_order,
                ui::combat_ui::update_ability_panel,
                ui::combat_ui::ability_slot_click_system,
                ui::combat_ui::move_button_click_system,
                ui::combat_ui::update_move_button,
                ui::hex_grid::hex_hover_system,
                ui::hex_grid::update_hex_visuals,
                ui::hex_grid::update_hex_tooltip,
                ui::hex_grid::hex_click_target,
                ui::hex_grid::update_token_positions,
                ui::log_ui::update_log,
                ui::log_ui::log_scroll_input,
                ui::log_ui::log_scrollbar_update,
                ui::console_log::print_log_system,
            )
                .run_if(in_state(AppState::Combat)),
        )
        // ── Animation systems (run independently, not in chain) ─────────
        .add_systems(
            Update,
            (
                ui::animation::process_animation_queue,
                ui::animation::animate_movement,
                ui::animation::enemy_popup_input,
            )
                .run_if(in_state(AppState::Combat)),
        )
        // ── Scenario advancement (always active) ─────────────────────────
        .add_systems(
            Update,
            scenario::input::victory_input_system.run_if(in_state(CombatPhase::Victory)),
        )
        .add_systems(
            Update,
            scenario::input::defeat_input_system.run_if(in_state(CombatPhase::Defeat)),
        )
        .add_systems(
            Update,
            scenario::advance_scenario_system
                .after(scenario::input::victory_input_system)
                .after(ui::story_ui::story_input_system),
        )
        // ── Combat pipeline ──────────────────────────────────────────────
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
                combat::movement::movement_system,
                combat::validation::validate_action_system,
                combat::resolution::resolve_action_system,
                combat::apply_effects::apply_effects_system,
                combat::enemy_popup::queue_enemy_popup,
                combat::advance_turn::advance_turn_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand))
                .run_if(ui::animation::combat_ready),
        )
        .run();
}
