use crate::game::components::{ActiveCombatant, Energy, Mana};
use crate::game::combat_log::{CombatEvent, CombatLog};
use bevy::prelude::*;

/// Runs at the start of every AwaitCommand frame.
/// Fires once per turn: when active combatant differs from last seen.
pub fn turn_start_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut mana_query: Query<&mut Mana>,
    mut energy_query: Query<&mut Energy>,
    mut log: ResMut<CombatLog>,
    mut last_active: Local<Option<Entity>>,
) {
    let current = active_q.single().ok();
    if current == *last_active {
        return;
    }
    *last_active = current;

    let Some(actor) = current else { return };

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

    // Energy: restore 1 at the start of the actor's own turn.
    if let Ok(mut energy) = energy_query.get_mut(actor) {
        if energy.current < energy.max {
            let current = energy.restore(1);
            log.push(CombatEvent::EnergyChanged {
                actor,
                current,
                max: energy.max,
            });
        }
    }
}
