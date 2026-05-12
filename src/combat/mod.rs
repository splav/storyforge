pub mod actions;
pub mod advance_turn;
pub mod ai;
pub mod engine_bridge;
pub mod apply_effects;
pub mod auras;
pub mod command_input;
pub mod effects_math;
pub mod effects_outcome;
pub mod effects_state;
pub mod enemy_popup;
pub mod movement;
pub mod phases;
pub mod pipeline;
pub mod resolution;
pub mod spawn;
pub mod skip_dead;
pub mod status_tick;
pub mod turn_order;
pub mod turn_start;
pub mod validation;

use crate::app_state::AppState;
use crate::game::components::ActiveCombatant;
use crate::game::messages::StartCombat;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::CombatContext;
use bevy::prelude::*;

/// Logical phases of the AwaitCommand combat pipeline.
/// Ordered via `.chain()` in main: TurnStart → Command → Execute → Finalize.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum CombatStep {
    TurnStart, // turn_start → skip_dead → skip_stunned
    Command,   // pact_ai → player_command ‖ enemy_ai
    Execute,   // movement → validate → resolve → apply_effects
    Finalize,  // queue_enemy_popup ‖ advance_turn
}

/// Listens for StartCombat events while in Overworld and transitions to Combat.
pub fn start_combat_system(
    mut commands: Commands,
    mut events: MessageReader<StartCombat>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next: ResMut<NextState<AppState>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
) {
    for ev in events.read() {
        ctx.round = 0;
        ctx.encounter = Some(ev.encounter);
        for e in &active_q { commands.entity(e).remove::<ActiveCombatant>(); }
        log.0.clear();
        log.push(CombatEvent::CombatStarted);
        next.set(AppState::Combat);
    }
}
