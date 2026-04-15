use crate::content::abilities::AoEShape;
use crate::core::ResourceKind;
use crate::game::components::{Abilities, ActionPoints, ActiveCombatant, Energy, Mana, Rage, Vital};
use crate::game::hex::hex_distance;
use crate::game::messages::{UseAbility, ValidatedAction};
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;

pub fn validate_action_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    mut events: MessageReader<UseAbility>,
    actors: Query<(
        &Vital,
        &ActionPoints,
        &Abilities,
        Option<&Rage>,
        Option<&Mana>,
        Option<&Energy>,
    )>,
    targets: Query<&Vital>,
    mut validated: MessageWriter<ValidatedAction>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        let (valid, disadvantage) = check(ev, active, &db, &positions, &actors, &targets);
        if !valid {
            continue;
        }
        validated.write(ValidatedAction {
            actor: ev.actor,
            ability: ev.ability.clone(),
            target: ev.target,
            target_pos: ev.target_pos,
            disadvantage,
        });
    }
}

fn check(
    ev: &UseAbility,
    active: Option<Entity>,
    db: &GameDb,
    positions: &HexPositions,
    actors: &Query<(
        &Vital,
        &ActionPoints,
        &Abilities,
        Option<&Rage>,
        Option<&Mana>,
        Option<&Energy>,
    )>,
    targets: &Query<&Vital>,
) -> (bool, bool) {
    if active != Some(ev.actor) {
        return (false, false);
    }

    let Ok((vital, ap, abilities, rage, mana, energy)) = actors.get(ev.actor) else {
        return (false, false);
    };
    if !vital.is_alive() || !ap.action {
        return (false, false);
    }
    if !abilities.0.contains(&ev.ability) {
        return (false, false);
    }

    let mut disadvantage = false;

    let Some(def) = db.abilities.get(&ev.ability) else {
        return (false, false);
    };

    // Check all resource costs.
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => vital.hp,
            ResourceKind::Mana => mana.map_or(0, |m| m.current),
            ResourceKind::Rage => rage.map_or(0, |r| r.current),
            ResourceKind::Energy => energy.map_or(0, |e| e.current),
        };
        if available < cost.amount {
            return (false, false);
        }
    }

    let is_aoe = def.aoe != AoEShape::None;

    if def.range.max == 0 {
        if ev.actor != ev.target {
            return (false, false);
        }
    } else if let Some(actor_pos) = positions.get(&ev.actor) {
        // Use target_pos for range check (works for both entity and cell targeting).
        let dist = hex_distance(actor_pos.0, actor_pos.1, ev.target_pos.0, ev.target_pos.1);
        if dist > def.range.max as i32 {
            return (false, false);
        }
        if dist < def.range.min as i32 {
            disadvantage = true;
        }
    }

    // For non-AoE, the primary target must be alive.
    if !is_aoe {
        let Ok(target_vital) = targets.get(ev.target) else {
            return (false, false);
        };
        if !target_vital.is_alive() {
            return (false, false);
        }
    }
    // For AoE, clicking an empty cell is valid — no entity check needed.

    (true, disadvantage)
}
