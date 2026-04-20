//! Snapshot→`MovementEnv` adapter for the AI planner.
//!
//! Both the beam-search generator (which walks a `SimState`) and the fallback
//! move handler start from a `BattleSnapshot`; they share this adapter so the
//! "which tiles block pass-through / stopping" translation lives once.
//!
//! The BFS itself lives in `game/pathfinding::reach_from`; this file just
//! builds the env and delegates. Kept here (rather than in pathfinding) so
//! the shared layer stays ignorant of `BattleSnapshot` / `UnitSnapshot`.

use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::game::hex::Hex;
use crate::game::pathfinding::{reach_from as reach_from_env, MovementEnv, ReachableMap};
use std::collections::HashSet;

/// BFS from `actor.pos` with the planner's passability / stop rules:
///
/// - Enemy tiles block **pass-through** (walked through but not landable).
/// - Every non-actor tile (enemy, ally, corpse via `blocked_tiles`) blocks
///   **stopping**. The actor's own tile stays legal so a zero-MP reach still
///   includes it.
pub fn reach_from(
    snap: &BattleSnapshot,
    actor: &UnitSnapshot,
    blocked_tiles: &HashSet<Hex>,
) -> ReachableMap {
    let enemy_positions: HashSet<Hex> = snap
        .enemies_of(actor.team)
        .map(|u| u.pos)
        .collect();
    let mut stop_blockers: HashSet<Hex> = snap
        .units
        .iter()
        .filter(|u| u.entity != actor.entity)
        .map(|u| u.pos)
        .collect();
    stop_blockers.extend(blocked_tiles.iter().copied());

    let env = MovementEnv { enemy_positions, stop_blockers };
    reach_from_env(actor.pos, actor.movement_points, &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::unit;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    #[test]
    fn neighbour_tile_is_reachable_with_mp() {
        // Actor has MP 3 → every empty neighbour should be in destinations.
        // (destinations excludes the start tile by design.)
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let s = snap(vec![actor.clone()]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        let neighbour = hex_from_offset(4, 3);
        assert!(
            reach.destinations.contains(&neighbour),
            "empty neighbour should be reachable (got {:?})",
            reach.destinations,
        );
    }

    #[test]
    fn enemy_tile_is_not_stoppable() {
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let enemy_pos = hex_from_offset(4, 3);
        let enemy = unit(2, Team::Player, enemy_pos);
        let s = snap(vec![actor.clone(), enemy]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        assert!(
            !reach.destinations.contains(&enemy_pos),
            "occupied enemy tile must not be a stop destination",
        );
    }

    #[test]
    fn ally_tile_is_not_stoppable_but_passable() {
        // Teammate blocks stopping on their tile but lets the actor pass
        // through when pathing to something beyond.
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let ally_pos = hex_from_offset(4, 3);
        let ally = unit(2, Team::Enemy, ally_pos);
        let s = snap(vec![actor.clone(), ally]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        assert!(
            !reach.destinations.contains(&ally_pos),
            "can't stop on ally",
        );
        // Beyond the ally should still be reachable via pass-through.
        let beyond = hex_from_offset(5, 3);
        assert!(
            reach.destinations.contains(&beyond),
            "beyond-ally tile should be reachable",
        );
    }

    #[test]
    fn blocked_tile_is_rejected_as_stop() {
        // A corpse (present in blocked_tiles but not in snapshot.units)
        // must still be treated as occupied for stopping.
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let corpse = hex_from_offset(4, 3);
        let s = snap(vec![actor.clone()]);
        let mut blocked = HashSet::new();
        blocked.insert(corpse);
        let reach = reach_from(&s, &actor, &blocked);
        assert!(
            !reach.destinations.contains(&corpse),
            "corpse tile must not be a stop destination",
        );
    }
}
