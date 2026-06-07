use bevy::prelude::*;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat;
use storyforge::combat::pipeline::CombatPipelinePlugin;
use storyforge::combat::CombatStep;
use storyforge::persistence::{detect_paths, settings_repo, PersistencePlugin};
use storyforge::combat::DiceRngRes;
use storyforge::game::messages::{ActionInput, RestartCombat, StartCombat};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective, GameDb, HexCorpses, HexPositions, PresetInitiative, SelectionState, TurnQueue, UiDirty};
use storyforge::combat::ai::config::tuning::AiTuning;
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
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<CombatEnvironment>()
        .init_resource::<storyforge::content::content_view::ActiveContent>()
        .init_resource::<TurnQueue>()
        .init_resource::<PresetInitiative>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .init_resource::<SelectionState>()
        .init_resource::<DiceRngRes>()
        .insert_resource(settings.difficulty.clone())
        .insert_resource(combat::ai::log::debug::AiDebugState {
            ai_debug: settings.ai_debug,
            ..Default::default()
        })
        .init_resource::<combat::ai::world::reservations::Reservations>()
        .init_resource::<combat::ai::log::AiLogger>()
        .init_resource::<combat::ai::log::PendingAiLogEntries>()
        .init_resource::<combat::ai::log::engine_trace::EngineTraceWriter>()
        .init_resource::<combat::ai::world::influence::InfluenceConfig>()
        .init_resource::<AiTuning>()
        .insert_resource(settings)
        .init_resource::<ui::console_log::ConsoleCursor>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<ui::hex_grid::HexHover>()
        .init_resource::<ui::hex_grid::HexLastClick>()
        .init_resource::<AnimationQueue>()
        .init_resource::<combat::enemy_popup::PopupCursor>()
        .init_resource::<UiDirty>()
        .init_resource::<ui::modal::PendingPrompt>()
        .init_resource::<ui::settings_ui::SettingsRebuild>()
        .add_message::<StartCombat>()
        .add_message::<ActionInput>()
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
        .add_systems(
            OnEnter(AppState::Story),
            (ui::story_ui::setup_story_screen, ui::story_ui::setup_choice_screen),
        )
        .add_systems(
            Update,
            (
                ui::story_ui::story_input_system,
                ui::story_ui::choice_input_system,
            )
                .run_if(in_state(AppState::Story)),
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
                combat::ai::log::open_combat_logs_on_combat_enter,
            ),
        )
        .add_systems(
            OnExit(AppState::Combat),
            (
                scenario::combat_scene::despawn_combatants,
                combat::ai::log::close_ai_log_on_combat_exit,
                combat::ai::log::close_engine_trace_on_combat_exit,
            ),
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
                ui::ability_panel::end_turn_button_system,
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
                combat::ai::log::debug::toggle_debug_system,
                combat::ai::log::debug::print_ai_debug_system
                    .after(CombatStep::Command),
                combat::ai::log::debug::debug_overlay_system
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
        // Write on_victory_flags into CampaignState before the player advances
        // (and before autosave fires in advance_scenario_system).
        .add_systems(
            OnEnter(CombatPhase::Victory),
            (scenario::write_victory_flags, scenario::write_objective_flags),
        )
        .add_systems(
            Update,
            scenario::input::victory_input_system.run_if(in_state(CombatPhase::Victory)),
        )
        .add_systems(
            OnEnter(CombatPhase::Defeat),
            (ui::combat_ui::setup_defeat_overlay, scenario::write_objective_flags),
        )
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
                .after(ui::story_ui::choice_input_system)
                .after(ui::combat_ui::defeat_overlay_input)
                .run_if(
                    in_state(AppState::Story)
                        .or(in_state(CombatPhase::Victory))
                        .or(in_state(CombatPhase::Defeat)),
                ),
        )
        // ── Combat pipeline (plugin) ─────────────────────────────────────
        .add_plugins(CombatPipelinePlugin)
        .run();
}
