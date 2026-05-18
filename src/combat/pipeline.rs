//! Декларативная регистрация combat pipeline.
//!
//! Порядок систем: StartRound → (TurnStart → Command → Execute → Finalize).
//! Plugin инкапсулирует `configure_sets` и `add_systems`, чтобы `main.rs`
//! не знал о внутренней раскладке боевых фаз.

use bevy::prelude::*;

use crate::app_state::{AppState, CombatPhase};
use crate::combat::engine_bridge::{
    CombatStateRes, UnitIdMap, apply_phase_transitions_system, engine_turn_start_system,
    init_state_from_ecs, PendingPhaseTransitions, process_action_system, project_state_to_ecs,
};
use crate::ui;

use super::{
    advance_turn, command_input, enemy_popup,
    start_combat_system, turn_order,
    CombatStep,
};

pub struct CombatPipelinePlugin;

impl Plugin for CombatPipelinePlugin {
    fn build(&self, app: &mut App) {
        // Engine state resources (Phase 0: transitional, ECS still authoritative).
        app.init_resource::<CombatStateRes>()
            .init_resource::<UnitIdMap>()
            .init_resource::<PendingPhaseTransitions>();

        // Initialize engine state once per round (on enter AwaitCommand).
        // Engine trace init runs immediately after, so it sees the fresh state.
        app.add_systems(
            OnEnter(CombatPhase::AwaitCommand),
            (init_state_from_ecs, crate::combat::ai::log::write_engine_trace_init_system).chain(),
        );

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
            engine_turn_start_system
                .in_set(CombatStep::TurnStart),
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
            (process_action_system, project_state_to_ecs, apply_phase_transitions_system)
                .chain()
                .in_set(CombatStep::Execute),
        )
        .add_systems(
            Update,
            (
                enemy_popup::queue_enemy_popup,
                advance_turn::advance_turn_system,
                advance_turn::check_victory_system,
            )
                .chain()
                .in_set(CombatStep::Finalize),
        );
    }
}
