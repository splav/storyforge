use crate::app_state::CombatPhase;
use crate::combat::ai::world::reservations::Reservations;
use crate::game::resources::CombatContext;
use bevy::prelude::*;


pub fn build_turn_order(
    mut ctx: ResMut<CombatContext>,
    mut reservations: ResMut<Reservations>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    ctx.round += 1;
    reservations.clear();
    // Round-1 initiative rolling + order construction is owned by the engine
    // (`bootstrap_combat_state` calls `roll_initiative_for_all` + `reconcile_turn_order`).
    // Round-2+ order stays as round-1's reconciled order (no new units without summons).
    next_phase.set(CombatPhase::AwaitCommand);
}
