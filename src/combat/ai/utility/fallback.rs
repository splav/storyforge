//! Fallback moves when plan generation produces no usable plans (normally
//! only when the actor is dead/missing from the snapshot). Close-in or
//! retreat, depending on HP.

use super::{AiDecision, MoveOrigin};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::reach_from;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::game::hex::Hex;

pub(super) fn fallback_move(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> AiDecision {
    if active.movement_points == 0 {
        return AiDecision::EndTurn;
    }

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    if enemies.is_empty() {
        return AiDecision::EndTurn;
    }

    let reach = reach_from(snap, active);

    // LOW_HP: retreat to the tile with lowest danger.
    if active.tags.contains(AiTags::LOW_HP) {
        let safest = reach
            .destinations
            .iter()
            .min_by(|a, b| {
                maps.danger
                    .get(**a)
                    .partial_cmp(&maps.danger.get(**b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        if let Some(dest) = safest {
            if let Some(path) = reach.path_to(dest) {
                if !path.is_empty() {
                    return AiDecision::Move { path, origin: MoveOrigin::Fallback };
                }
            }
        }
        return AiDecision::EndTurn;
    }

    // Normal: find reachable tile closest to any enemy.
    let mut best: Option<(Hex, u32)> = None;
    for &cell in &reach.destinations {
        for enemy in &enemies {
            let dist = cell.unsigned_distance_to(enemy.pos);
            if best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((cell, dist));
            }
        }
    }

    if let Some((dest, _)) = best {
        if let Some(path) = reach.path_to(dest) {
            if !path.is_empty() {
                return AiDecision::Move { path, origin: MoveOrigin::Fallback };
            }
        }
    }

    AiDecision::EndTurn
}
