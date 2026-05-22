//! Snapshot→`MovementEnv` adapter for the AI planner.
//!
//! Both the beam-search generator (which walks a `SimState`) and the fallback
//! move handler start from a `BattleSnapshot`; they share this adapter so the
//! "which tiles block pass-through / stopping" translation lives once.
//!
//! The BFS itself lives in `game/pathfinding::reach_from`; this file just
//! builds the env and delegates. Kept here (rather than in pathfinding) so
//! the shared layer stays ignorant of `BattleSnapshot` / `UnitSnapshot`.

use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::game::hex::Hex;
use crate::game::pathfinding::{reach_from as reach_from_env, MovementEnv, ReachableMap};
use std::collections::HashSet;

/// BFS from `actor.pos` with the planner's passability / stop rules:
///
/// - **Pass-through**: all enemies (live or dead) block. Only live allies
///   are pass-through-friendly — walking through a corpse is forbidden
///   to prevent paths that trigger AoOs from units the snapshot thought
///   were dead.
/// - **Stopping**: every non-actor occupant blocks — live enemy, live
///   ally, and corpse (hp=0 unit still in the snapshot). The actor's own
///   tile stays legal so a zero-MP reach includes it.
///
/// Single source of truth is `snap.state.units()` now that corpses live there
/// instead of in a parallel `blocked_tiles` channel.
pub fn reach_from(snap: &BattleSnapshot, actor: UnitView<'_>) -> ReachableMap {
    let enemy_positions: HashSet<Hex> = snap
        .all_enemies_of(actor.team)
        .map(|u| u.pos)
        .collect();
    let stop_blockers: HashSet<Hex> = snap
        .state
        .units()
        .iter()
        .filter_map(|u| {
            let e = snap.entity_for_uid(u.id)?;
            if e != actor.entity() { Some(u.pos) } else { None }
        })
        .collect();

    let env = MovementEnv { enemy_positions, stop_blockers };
    reach_from_env(actor.pos, actor.movement_points, &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{unit, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::world::snapshot::UnitSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        snapshot_from(units, 1)
    }

    #[test]
    fn neighbour_tile_is_reachable_with_mp() {
        // Actor has MP 3 → every empty neighbour should be in destinations.
        // (destinations excludes the start tile by design.)
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let s = snap(vec![actor.clone()]);
        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);
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
        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);
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
        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);
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

    /// Corpses (hp=0 enemy units) fully block the tile — no stopping and no
    /// pass-through. This prevents paths that would route through a tile the
    /// planner thinks is clear but that triggers an AoO at execution time.
    #[test]
    fn corpse_tile_is_fully_blocked() {
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let corpse_pos = hex_from_offset(4, 3);
        let corpse = UnitBuilder::new(2, Team::Player, corpse_pos).hp(0).build();
        let s = snap(vec![actor.clone(), corpse]);
        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);
        // Can't stop on the corpse tile.
        assert!(
            !reach.destinations.contains(&corpse_pos),
            "corpse tile must not be a stop destination",
        );
        // BFS never visits the corpse tile (it's impassable), so there's no
        // reconstructible path through it even as an intermediate step.
        assert!(
            reach.path_to(corpse_pos).is_none(),
            "corpse tile must be impassable — BFS should not have explored it",
        );
    }
}
