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
        // Hex tiebreak: HashSet iteration is randomized per-process; without
        // a deterministic secondary sort, ties in danger flip across processes.
        let safest = reach
            .destinations
            .iter()
            .min_by(|a, b| {
                maps.danger
                    .get(**a)
                    .partial_cmp(&maps.danger.get(**b))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| (a.x, a.y).cmp(&(b.x, b.y)))
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
    // Sort destinations first — HashSet iteration is randomized per-process,
    // and `dist < bd` (strict less-than) keeps the FIRST seen tile on ties,
    // so randomized order would pick different tile per process.
    let mut sorted_dests: Vec<Hex> = reach.destinations.iter().copied().collect();
    sorted_dests.sort_by(|a, b| (a.x, a.y).cmp(&(b.x, b.y)));
    let mut best: Option<(Hex, u32)> = None;
    for cell in sorted_dests {
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
