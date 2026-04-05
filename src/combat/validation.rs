use bevy::prelude::*;
use crate::game::components::{Abilities, ActionPoints, Vital};
use crate::game::messages::{UseAbility, ValidatedAction};
use crate::game::resources::CombatContext;

/// Validates incoming UseAbility messages and forwards valid ones as ValidatedAction.
/// Runs in the same chained system set as player_command and resolve — same frame, no cross-frame message loss.
pub fn validate_action_system(
    ctx: Res<CombatContext>,
    mut events: MessageReader<UseAbility>,
    actors: Query<(&Vital, &ActionPoints, &Abilities)>,
    targets: Query<&Vital>,
    mut validated: MessageWriter<ValidatedAction>,
) {
    for ev in events.read() {
        if !is_valid(ev, &ctx, &actors, &targets) {
            continue;
        }
        validated.write(ValidatedAction { actor: ev.actor, ability: ev.ability, target: ev.target });
    }
}

fn is_valid(
    ev: &UseAbility,
    ctx: &CombatContext,
    actors: &Query<(&Vital, &ActionPoints, &Abilities)>,
    targets: &Query<&Vital>,
) -> bool {
    if ctx.active != Some(ev.actor) {
        return false;
    }
    let Ok((vital, ap, abilities)) = actors.get(ev.actor) else {
        return false;
    };
    if !vital.is_alive() || !ap.action {
        return false;
    }
    if !abilities.0.contains(&ev.ability) {
        return false;
    }
    let Ok(target_vital) = targets.get(ev.target) else {
        return false;
    };
    target_vital.is_alive()
}
