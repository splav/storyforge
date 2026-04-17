use bevy::prelude::*;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat;
use storyforge::combat::CombatStep;
use storyforge::persistence::{detect_paths, settings_repo, PersistencePlugin};
use storyforge::core::DiceRng;
use storyforge::game::messages::{
    ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, MoveUnit, RestartCombat, SpawnUnit, StartCombat,
    UseAbility, ValidatedAction,
};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{CombatContext, CombatObjective, GameDb, HexPositions, PresetInitiative, SelectionState, TurnQueue, UiDirty};
use storyforge::scenario;
use storyforge::ui;
use storyforge::ui::animation::AnimationQueue;

fn main() {
    let paths = detect_paths();
    let settings = settings_repo::load_layered(paths.as_ref());

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Storyforge".into(),
                resolution: (900u32, 600u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(PersistencePlugin { paths: paths.clone() })
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatObjective>()
        .init_resource::<storyforge::content::content_view::ActiveContent>()
        .init_resource::<TurnQueue>()
        .init_resource::<PresetInitiative>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<DiceRng>()
        .insert_resource(settings.difficulty.clone())
        .insert_resource(combat::ai::debug::AiDebugState {
            ai_debug: settings.ai_debug,
            ..Default::default()
        })
        .init_resource::<combat::ai::reservations::Reservations>()
        .insert_resource(settings)
        .init_resource::<ui::console_log::ConsoleCursor>()
        .init_resource::<HexPositions>()
        .init_resource::<ui::hex_grid::HexHover>()
        .init_resource::<ui::hex_grid::HexLastClick>()
        .init_resource::<AnimationQueue>()
        .init_resource::<combat::enemy_popup::PopupCursor>()
        .init_resource::<UiDirty>()
        .init_resource::<ui::modal::PendingPrompt>()
        .init_resource::<ui::settings_ui::SettingsRebuild>()
        .add_message::<StartCombat>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<MoveUnit>()
        .add_message::<EndTurn>()
        .add_message::<RestartCombat>()
        .add_message::<SpawnUnit>()
        .add_message::<scenario::AdvanceScenario>()
        .add_systems(
            Startup,
            (
                scenario::start_scenario,
                ui::combat_ui::setup_hud,
                ui::hex_grid::setup_hex_grid,
            ),
        )
        // ── Shared button hover effect (runs in all states) ──────────────
        .add_systems(Update, ui::button::button_hover_system)
        // ── Main menu ────────────────────────────────────────────────────
        .add_systems(OnEnter(AppState::MainMenu), ui::main_menu_ui::setup_main_menu)
        .add_systems(
            Update,
            (
                ui::main_menu_ui::campaign_button_system,
                ui::main_menu_ui::continue_button_system,
                ui::main_menu_ui::settings_button_system,
            )
                .run_if(in_state(AppState::MainMenu)),
        )
        .add_systems(OnExit(AppState::MainMenu), ui::main_menu_ui::cleanup_main_menu)
        // ── Settings ────────────────────────────────────────────────────
        .add_systems(OnEnter(AppState::Settings), ui::settings_ui::setup_settings)
        .add_systems(
            Update,
            (
                ui::settings_ui::difficulty_button_system,
                ui::settings_ui::slot_action_system,
                ui::settings_ui::back_button_system,
                ui::settings_ui::rebuild_settings_if_needed,
            )
                .run_if(in_state(AppState::Settings)),
        )
        .add_systems(OnExit(AppState::Settings), ui::settings_ui::cleanup_settings)
        // ── Modal (runs in all states) ──────────────────────────────────
        .add_systems(Update, (ui::modal::sync_modal, ui::modal::handle_modal_input))
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
                ui::ability_panel::update_ability_panel,
                ui::ability_panel::update_ability_description,
                ui::ability_panel::ability_slot_click_system,
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
        // ── AI debug overlay ────────────────────────────────────────────
        .add_systems(
            Update,
            (
                combat::ai::debug::toggle_debug_system,
                combat::ai::debug::print_ai_debug_system
                    .after(CombatStep::Command),
                combat::ai::debug::debug_overlay_system
                    .after(ui::hex_grid::update_hex_visuals),
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
        .add_systems(OnEnter(CombatPhase::Defeat), ui::combat_ui::setup_defeat_overlay)
        .add_systems(OnExit(CombatPhase::Defeat), ui::combat_ui::cleanup_defeat_overlay)
        .add_systems(
            Update,
            (
                ui::combat_ui::defeat_overlay_input,
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
        // ── TurnStart: init → skip dead → skip stunned → refresh auras ─────────
        .add_systems(
            Update,
            (
                combat::turn_start::turn_start_system,
                combat::skip_dead::skip_dead_turn_system,
                combat::skip_dead::skip_stunned_turn_system,
                combat::auras::apply_auras_system,
            )
                .chain()
                .in_set(CombatStep::TurnStart),
        )
        // ── Command: player & enemy input (parallel branches) ──────
        .add_systems(
            Update,
            (
                combat::ai::enemy_turn::pact_ai_system,
                combat::command_input::player_command_system,
            )
                .chain()
                .in_set(CombatStep::Command),
        )
        .add_systems(
            Update,
            combat::ai::enemy_turn::enemy_ai_system
                .in_set(CombatStep::Command),
        )
        // ── Execute: movement → validation → resolution → effects → spawn → phases ──
        .add_systems(
            Update,
            (
                combat::movement::movement_system,
                combat::validation::validate_action_system,
                combat::resolution::resolve_action_system,
                combat::apply_effects::apply_effects_system,
                combat::spawn::apply_spawn_system,
                combat::phases::phase_transition_system,
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
