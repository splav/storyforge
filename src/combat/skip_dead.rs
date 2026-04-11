use crate::game::components::{ActionPoints, Dead, StatusEffects};
use crate::game::messages::EndTurn;
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, GameDb};
use bevy::prelude::*;

/// If the active combatant is dead, immediately end their turn.
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

/// If the active combatant is stunned (has a status with skips_turn), skip their turn.
/// Sets ap.action = false so that enemy_ai's UseAbility is rejected by validation.
pub fn skip_stunned_turn_system(
    ctx: Res<CombatContext>,
    statuses: Query<&StatusEffects>,
    mut action_points: Query<&mut ActionPoints>,
    db: Res<GameDb>,
    mut log: ResMut<CombatLog>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let Some(actor) = ctx.active else { return };
    let Ok(se) = statuses.get(actor) else { return };
    let is_stunned =
        se.0.iter()
            .any(|s| db.statuses.get(&s.id).map_or(false, |def| def.skips_turn));
    if is_stunned {
        if let Ok(mut ap) = action_points.get_mut(actor) {
            ap.action = false;
        }
        log.push(CombatEvent::TurnSkipped { actor });
        end_turn.write(EndTurn { actor });
    }
}
