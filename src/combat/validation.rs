use crate::game::components::{Abilities, ActionPoints, Mana, Rage, Vital};
use crate::game::hex::hex_distance;
use crate::game::messages::{UseAbility, ValidatedAction};
use crate::game::resources::{CombatContext, GameDb, HexPositions};
use bevy::prelude::*;

pub fn validate_action_system(
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    mut events: MessageReader<UseAbility>,
    actors: Query<(
        &Vital,
        &ActionPoints,
        &Abilities,
        Option<&Rage>,
        Option<&Mana>,
    )>,
    targets: Query<&Vital>,
    mut validated: MessageWriter<ValidatedAction>,
) {
    for ev in events.read() {
        let valid = is_valid(ev, &ctx, &db, &positions, &actors, &targets);
        info!(
            "[VALID] UseAbility: actor={:?}, ability={:?}, target={:?} → valid={}",
            ev.actor, ev.ability, ev.target, valid
        );
        if !valid {
            continue;
        }
        validated.write(ValidatedAction {
            actor: ev.actor,
            ability: ev.ability.clone(),
            target: ev.target,
        });
    }
}

fn is_valid(
    ev: &UseAbility,
    ctx: &CombatContext,
    db: &GameDb,
    positions: &HexPositions,
    actors: &Query<(
        &Vital,
        &ActionPoints,
        &Abilities,
        Option<&Rage>,
        Option<&Mana>,
    )>,
    targets: &Query<&Vital>,
) -> bool {
    if ctx.active != Some(ev.actor) {
        info!("[VALID]   FAIL: ctx.active={:?} != actor={:?}", ctx.active, ev.actor);
        return false;
    }

    let Ok((vital, ap, abilities, rage, mana)) = actors.get(ev.actor) else {
        info!("[VALID]   FAIL: actor query failed");
        return false;
    };
    if !vital.is_alive() || !ap.action {
        info!("[VALID]   FAIL: alive={} ap.action={}", vital.is_alive(), ap.action);
        return false;
    }
    if !abilities.0.contains(&ev.ability) {
        info!("[VALID]   FAIL: ability {:?} not in {:?}", ev.ability, abilities.0);
        return false;
    }

    if let Some(def) = db.abilities.get(&ev.ability) {
        if def.rage_cost > 0 && rage.map_or(0, |r| r.current) < def.rage_cost {
            return false;
        }
        if def.mana_cost > 0 && mana.map_or(0, |m| m.current) < def.mana_cost {
            return false;
        }

        // Range check (skip for self-targeted / range-0 abilities).
        if def.range > 0 {
            if let (Some(actor_pos), Some(target_pos)) =
                (positions.get(&ev.actor), positions.get(&ev.target))
            {
                if hex_distance(actor_pos.0, actor_pos.1, target_pos.0, target_pos.1)
                    > def.range as i32
                {
                    return false;
                }
            }
        }
    }

    let Ok(target_vital) = targets.get(ev.target) else {
        return false;
    };
    target_vital.is_alive()
}
