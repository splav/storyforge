pub mod advance_turn;
pub mod ai;
pub mod dice_resource;
pub mod engine_bridge;
pub mod command_input;
pub mod effects_outcome;
pub mod effects_state;
pub mod enemy_popup;
pub mod legality_adapter;
pub mod pipeline;
pub mod turn_order;

pub use legality_adapter::BevyActions;

pub use dice_resource::DiceRngRes;

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
    Execute,   // process_action → project_state → phases
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
