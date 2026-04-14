pub mod advance_turn;
pub mod ai_difficulty;
pub mod ai_scoring;
pub mod apply_effects;
pub mod command_input;
pub mod enemy_ai;
pub mod enemy_popup;
pub mod movement;
pub mod resolution;
pub mod skip_dead;
pub mod turn_order;
pub mod turn_start;
pub mod validation;

use crate::app_state::AppState;
use crate::game::components::ActiveCombatant;
use crate::game::messages::StartCombat;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::CombatContext;
use bevy::prelude::*;

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
