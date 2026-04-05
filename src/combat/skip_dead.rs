use bevy::prelude::*;
use crate::game::components::Dead;
use crate::game::messages::EndTurn;
use crate::game::resources::CombatContext;

/// If the active combatant is dead, immediately end their turn so the
/// pipeline advances without waiting for player input or enemy AI.
pub fn skip_dead_turn_system(
    ctx: Res<CombatContext>,
    dead: Query<(), With<Dead>>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let Some(actor) = ctx.active else { return };
    if dead.get(actor).is_ok() {
        end_turn.write(EndTurn { actor });
    }
}
