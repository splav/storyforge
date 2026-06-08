//! Декларативная регистрация combat pipeline.
//!
//! Порядок систем: StartRound → (TurnStart → Command → Execute → Finalize).
//! Plugin инкапсулирует `configure_sets` и `add_systems`, чтобы `main.rs`
//! не знал о внутренней раскладке боевых фаз.

use bevy::prelude::*;

use crate::app_state::{AppState, CombatPhase};
use crate::combat::engine_bridge::{
    self as engine_bridge, apply_bridge_queues_post_projection, apply_bridge_queues_pre_projection,
    bootstrap_combat_state, process_action_system, project_state_to_ecs,
    reset_engine_mirrors_on_exit_combat, reset_engine_mirrors_on_restart, BridgeQueues,
    CombatStateRes, UnitIdMap,
};
use crate::ui;

use super::{
    advance_turn, command_input, enemy_popup, start_combat_system, turn_order, CombatStep,
};

pub struct CombatPipelinePlugin;

impl Plugin for CombatPipelinePlugin {
    fn build(&self, app: &mut App) {
        // Engine state resources.
        app.init_resource::<CombatStateRes>()
            .init_resource::<UnitIdMap>()
            .init_resource::<BridgeQueues>()
            .init_resource::<crate::game::resources::PhaseDeadline>();

        // Engine mirror teardown — combat plugin owns its own lifecycle:
        // - OnExit(AppState::Combat) covers normal Victory/Defeat → next combat.
        // - RestartCombat reader covers in-combat restart (which doesn't exit
        //   AppState::Combat). Bevy permits independent readers, so this
        //   coexists with restart_combat_system.
        app.add_systems(
            OnExit(AppState::Combat),
            reset_engine_mirrors_on_exit_combat,
        )
        .add_systems(Update, reset_engine_mirrors_on_restart);

        app.add_systems(
            Update,
            start_combat_system.run_if(in_state(AppState::Overworld)),
        )
        .add_systems(
            Update,
            (
                project_state_to_ecs,
                ui::hex_grid::assign_hex_positions,
                turn_order::build_turn_order,
                bootstrap_combat_state,
                crate::combat::ai::log::write_engine_trace_init_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::StartRound)),
        )
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
        .add_systems(
            Update,
            (
                super::ai::system::pact_ai_system,
                command_input::player_command_system,
            )
                .chain()
                .in_set(CombatStep::Command),
        )
        .add_systems(
            Update,
            super::ai::system::enemy_ai_system.in_set(CombatStep::Command),
        )
        .add_systems(
            Update,
            (
                process_action_system,
                apply_bridge_queues_pre_projection,
                project_state_to_ecs,
                apply_bridge_queues_post_projection,
                engine_bridge::apply_phase_overrides_system,
                super::ai::log::flush_pending_ai_log_system,
            )
                .chain()
                .in_set(CombatStep::Execute),
        )
        .add_systems(
            Update,
            (
                enemy_popup::queue_enemy_popup,
                advance_turn::check_victory_system,
                advance_turn::check_phase_deadline_system,
            )
                .chain()
                .in_set(CombatStep::Finalize),
        );
    }
}
