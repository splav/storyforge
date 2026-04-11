use crate::game::components::Mana;
use crate::game::resources::{CombatContext, CombatEvent, CombatLog};
use bevy::prelude::*;

/// Runs at the start of every AwaitCommand frame.
/// Fires once per turn: when ctx.active differs from ctx.last_active.
pub fn turn_start_system(
    mut ctx: ResMut<CombatContext>,
    mut mana_query: Query<&mut Mana>,
    mut log: ResMut<CombatLog>,
) {
    if ctx.active == ctx.last_active {
        return;
    }
    ctx.last_active = ctx.active;

    let Some(actor) = ctx.active else { return };

    // Mana: restore 1 at the start of the actor's own turn.
    if let Ok(mut mana) = mana_query.get_mut(actor) {
        if mana.current < mana.max {
            let current = mana.restore(1);
            log.push(CombatEvent::ManaChanged {
                actor,
                current,
                max: mana.max,
            });
        }
    }
}
