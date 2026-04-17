#![allow(clippy::too_many_arguments)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::content::abilities::AoEShape;
use crate::core::ResourceKind;
use crate::game::components::{ActiveCombatant, ValidationActorQ, Vital};
use crate::game::hex::in_bounds;
use crate::game::messages::{EndTurn, UseAbility, ValidatedAction};
use crate::game::resources::HexPositions;
use bevy::prelude::*;

pub fn validate_action_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    content: Res<ActiveContent>,
    positions: Res<HexPositions>,
    mut events: MessageReader<UseAbility>,
    actors: Query<ValidationActorQ>,
    targets: Query<&Vital>,
    mut validated: MessageWriter<ValidatedAction>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        let (valid, disadvantage) = check(ev, active, &content, &positions, &actors, &targets);
        if !valid {
            // Rejected action still ends the turn to prevent infinite loops.
            end_turn.write(EndTurn { actor: ev.actor });
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
    content: &ContentView,
    positions: &HexPositions,
    actors: &Query<ValidationActorQ>,
    targets: &Query<&Vital>,
) -> (bool, bool) {
    if active != Some(ev.actor) {
        return (false, false);
    }

    let Ok(a) = actors.get(ev.actor) else {
        return (false, false);
    };
    if !a.vital.is_alive() || !a.ap.action {
        return (false, false);
    }
    // Keyed (universal) abilities bypass class ability list check.
    let is_keyed = content.abilities.get(&ev.ability).is_some_and(|d| d.key.is_some());
    if !is_keyed && !a.abilities.0.contains(&ev.ability) {
        return (false, false);
    }

    let mut disadvantage = false;

    // Statuses that cause disadvantage on all rolls (e.g. "disoriented").
    // Short-range penalty below is a separate source that can set this flag too.
    if let Some(se) = a.statuses {
        if se
            .0
            .iter()
            .any(|s| content.statuses.get(&s.id).is_some_and(|d| d.causes_disadvantage))
        {
            disadvantage = true;
        }
    }

    let Some(def) = content.abilities.get(&ev.ability) else {
        return (false, false);
    };

    // Check all resource costs.
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => a.vital.hp,
            ResourceKind::Mana => a.mana.map_or(0, |m| m.current),
            ResourceKind::Rage => a.rage.map_or(0, |r| r.current),
            ResourceKind::Energy => a.energy.map_or(0, |e| e.current),
        };
        if available < cost.amount {
            return (false, false);
        }
    }

    // Check blocks_mana_abilities status (faith crit fail).
    let has_mana_cost = def.costs.iter().any(|c| c.resource == ResourceKind::Mana);
    if has_mana_cost {
        if let Some(se) = a.statuses {
            let blocked = se.0.iter().any(|s| {
                content.statuses.get(&s.id).is_some_and(|d| d.blocks_mana_abilities)
            });
            if blocked {
                return (false, false);
            }
        }
    }

    let is_aoe = def.aoe != AoEShape::None;

    if def.range.max == 0 {
        if ev.actor != ev.target {
            return (false, false);
        }
    } else if let Some(actor_pos) = positions.get(&ev.actor) {
        if !in_bounds(ev.target_pos) {
            return (false, false);
        }
        let dist = actor_pos.unsigned_distance_to(ev.target_pos);
        if dist > def.range.max {
            return (false, false);
        }
        if dist < def.range.min {
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
