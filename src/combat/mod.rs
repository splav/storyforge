pub mod cleanup;
pub mod command_input;
pub mod enemy_ai;
pub mod resolution;
pub mod skip_dead;
pub mod turn_order;
pub mod validation;

use bevy::prelude::*;
use crate::app_state::AppState;
use crate::game::messages::StartCombat;
use crate::game::resources::{CombatContext, CombatEvent, CombatLog};

/// Listens for StartCombat events while in Overworld and transitions to Combat.
pub fn start_combat_system(
    mut events: MessageReader<StartCombat>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next: ResMut<NextState<AppState>>,
) {
    for ev in events.read() {
        ctx.round = 0;
        ctx.encounter = Some(ev.encounter);
        ctx.active = None;
        log.0.clear();
        log.push(CombatEvent::CombatStarted);
        next.set(AppState::Combat);
        // CombatPhase::StartRound is the default, so it activates automatically.
    }
}
