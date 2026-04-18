use crate::content::content_view::ActiveContent;
use crate::game::components::{ActionPoints, ActiveCombatant, Dead, StatusEffects};
use crate::game::messages::EndTurn;
use crate::game::combat_log::{CombatEvent, CombatLog};
use bevy::prelude::*;

/// If the active combatant is dead, immediately end their turn.
pub fn skip_dead_turn_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    dead: Query<(), With<Dead>>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let Ok(actor) = active_q.single() else { return };
    if dead.get(actor).is_ok() {
        end_turn.write(EndTurn { actor });
    }
}

/// If the active combatant is stunned (has a status with skips_turn), skip their turn.
/// Sets ap.action = false so that enemy_ai's UseAbility is rejected by validation.
pub fn skip_stunned_turn_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    statuses: Query<&StatusEffects>,
    mut action_points: Query<&mut ActionPoints>,
    content: Res<ActiveContent>,
    mut log: ResMut<CombatLog>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(se) = statuses.get(actor) else { return };
    let is_stunned =
        se.0.iter()
            .any(|s| content.statuses.get(&s.id).is_some_and(|def| def.skips_turn));
    if is_stunned {
        if let Ok(mut ap) = action_points.get_mut(actor) {
            ap.action = false;
            ap.movement_points = 0;
        }
        log.push(CombatEvent::TurnSkipped { actor });
        end_turn.write(EndTurn { actor });
    }
}
