//! Fallback moves when plan generation produces no usable plans (normally
//! only when the actor is dead/missing from the snapshot). Close-in or
//! retreat, depending on HP.

use super::AiDecision;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::game::hex::{can_stop_on, is_passable, Hex};
use crate::game::pathfinding::{reachable_with_paths, ReachableMap};
use std::collections::HashSet;

pub(super) fn fallback_move(
    active: &UnitSnapshot,
    blocked_tiles: &HashSet<Hex>,
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

    let reach = build_fallback_reach(active, blocked_tiles, snap);

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
                    return AiDecision::MoveCloser { path };
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
                return AiDecision::MoveCloser { path };
            }
        }
    }

    AiDecision::EndTurn
}

/// BFS from `active.pos` with the same passability/stop rules as the planner.
/// Duplicates a small slice of `generator::build_reach`, but this path is only
/// hit in edge cases (actor missing from the snapshot) so sharing isn't worth
/// the plumbing.
fn build_fallback_reach(
    active: &UnitSnapshot,
    blocked_tiles: &HashSet<Hex>,
    snap: &BattleSnapshot,
) -> ReachableMap {
    let enemy_positions: HashSet<Hex> = snap
        .enemies_of(active.team)
        .map(|u| u.pos)
        .collect();
    let mut all_occupied: HashSet<Hex> = snap
        .units
        .iter()
        .filter(|u| u.entity != active.entity)
        .map(|u| u.pos)
        .collect();
    all_occupied.extend(blocked_tiles.iter().copied());

    reachable_with_paths(
        active.pos,
        active.movement_points,
        move |h| is_passable(h, &enemy_positions),
        move |h| can_stop_on(h, &all_occupied, None),
    )
}
