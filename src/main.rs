use bevy::prelude::*;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat;
use storyforge::combat::CombatStep;
use storyforge::core::DiceRng;
use storyforge::game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, MoveUnit, RestartCombat, StartCombat,
    UseAbility, ValidatedAction,
};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{CombatContext, GameDb, HexPositions, PresetInitiative, SelectionState, TurnQueue, UiDirty};
use storyforge::scenario;
use storyforge::ui;
use storyforge::ui::animation::AnimationQueue;

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
        .init_resource::<PresetInitiative>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<DiceRng>()
        .init_resource::<combat::ai_difficulty::DifficultyProfile>()
        .init_resource::<ui::console_log::ConsoleCursor>()
        .init_resource::<HexPositions>()
        .init_resource::<ui::hex_grid::HexHover>()
        .init_resource::<ui::hex_grid::HexLastClick>()
        .init_resource::<AnimationQueue>()
        .init_resource::<combat::enemy_popup::PopupCursor>()
        .init_resource::<UiDirty>()
        .add_message::<StartCombat>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<MoveUnit>()
        .add_message::<EndTurn>()
        .add_message::<RestartCombat>()
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
        .add_systems(OnEnter(AppState::Combat), scenario::combat_scene::spawn_combat_scene)
        .add_systems(
            OnExit(AppState::Combat),
            scenario::combat_scene::despawn_combatants,
        )
        // ── Combat UI (runs every frame during combat) ───────────────────
        .add_systems(
            Update,
            ui::hex_grid::ui_dirty_bridge.run_if(in_state(AppState::Combat)),
        )
        .add_systems(
            Update,
            (
                ui::combat_ui::update_phase_hint,
                ui::turn_order_ui::update_turn_order,
                ui::turn_order_ui::update_turn_order_hp,
                ui::turn_order_ui::update_turn_order_tooltip,
                ui::combat_ui::update_ability_panel,
                ui::combat_ui::ability_slot_click_system,
                ui::combat_ui::move_button_click_system,
                ui::combat_ui::update_move_button,
            )
                .after(ui::hex_grid::ui_dirty_bridge)
                .run_if(in_state(AppState::Combat)),
        )
        .add_systems(
            Update,
            (
                ui::hex_grid::hex_hover_system,
                ui::hex_grid::update_hex_visuals,
                ui::hex_grid::update_hex_tooltip,
                ui::hex_grid::hex_click_target,
                ui::hex_grid::update_token_positions,
                ui::log_ui::update_log,
                ui::log_ui::log_scroll_input,
                ui::log_ui::log_scrollbar_update,
            )
                .after(ui::hex_grid::ui_dirty_bridge)
                .run_if(in_state(AppState::Combat)),
        )
        .add_systems(
            Update,
            ui::console_log::print_log_system
                .after(ui::hex_grid::ui_dirty_bridge)
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
        .add_systems(OnEnter(CombatPhase::Defeat), ui::combat_ui::setup_defeat_overlay)
        .add_systems(OnExit(CombatPhase::Defeat), ui::combat_ui::cleanup_defeat_overlay)
        .add_systems(
            Update,
            (
                ui::combat_ui::defeat_overlay_input,
                ui::combat_ui::defeat_button_hover,
                scenario::combat_scene::restart_combat_system,
            )
                .run_if(in_state(CombatPhase::Defeat)),
        )
        .add_systems(
            Update,
            scenario::advance_scenario_system
                .after(scenario::input::victory_input_system)
                .after(ui::story_ui::story_input_system)
                .run_if(in_state(AppState::Story).or(in_state(CombatPhase::Victory))),
        )
        // ── Combat pipeline ──────────────────────────────────────────────
        .add_systems(
            Update,
            combat::start_combat_system.run_if(in_state(AppState::Overworld)),
        )
        .add_systems(
            Update,
            (
                ui::hex_grid::assign_hex_positions,
                combat::turn_order::build_turn_order,
            )
                .chain()
                .run_if(in_state(CombatPhase::StartRound)),
        )
        // ── Combat pipeline sets ────────────────────────────────────
        .configure_sets(
            Update,
            (
                CombatStep::TurnStart,
                CombatStep::Command,
                CombatStep::Execute,
                CombatStep::Finalize,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand))
                .run_if(ui::animation::combat_ready),
        )
        // ── TurnStart: init → skip dead → skip stunned ─────────────
        .add_systems(
            Update,
            (
                combat::turn_start::turn_start_system,
                combat::skip_dead::skip_dead_turn_system,
                combat::skip_dead::skip_stunned_turn_system,
            )
                .chain()
                .in_set(CombatStep::TurnStart),
        )
        // ── Command: player & enemy input (parallel branches) ──────
        .add_systems(
            Update,
            (
                combat::enemy_ai::pact_ai_system,
                combat::command_input::player_command_system,
            )
                .chain()
                .in_set(CombatStep::Command),
        )
        .add_systems(
            Update,
            combat::enemy_ai::enemy_ai_system
                .in_set(CombatStep::Command),
        )
        // ── Execute: movement → validation → resolution → effects ──
        .add_systems(
            Update,
            (
                combat::movement::movement_system,
                combat::validation::validate_action_system,
                combat::resolution::resolve_action_system,
                combat::apply_effects::apply_effects_system,
            )
                .chain()
                .in_set(CombatStep::Execute),
        )
        // ── Finalize: popup + advance (parallel) ────────────────────
        .add_systems(
            Update,
            (
                combat::enemy_popup::queue_enemy_popup,
                combat::advance_turn::advance_turn_system,
            )
                .in_set(CombatStep::Finalize),
        )
        .run();
}
