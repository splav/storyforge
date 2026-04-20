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
/// - **Pass-through**: only live enemies block (walked through but not
///   landable). Live allies and corpses are pass-through-friendly.
/// - **Stopping**: every non-actor occupant blocks — live enemy, live
///   ally, and corpse (hp=0 unit still in the snapshot). The actor's own
///   tile stays legal so a zero-MP reach includes it.
///
/// Single source of truth is `snap.units` now that corpses live there
/// instead of in a parallel `blocked_tiles` channel.
pub fn reach_from(snap: &BattleSnapshot, actor: &UnitSnapshot) -> ReachableMap {
    let enemy_positions: HashSet<Hex> = snap
        .enemies_of(actor.team)
        .map(|u| u.pos)
        .collect();
    let stop_blockers: HashSet<Hex> = snap
        .units
        .iter()
        .filter(|u| u.entity != actor.entity)
        .map(|u| u.pos)
        .collect();

    let env = MovementEnv { enemy_positions, stop_blockers };
    reach_from_env(actor.pos, actor.movement_points, &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{unit, UnitBuilder};
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
        let reach = reach_from(&s, &actor);
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
        let reach = reach_from(&s, &actor);
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
        let reach = reach_from(&s, &actor);
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

    /// Corpses are live in the snapshot with `hp = 0` (no more parallel
    /// `blocked_tiles` channel). The reach adapter must still treat their
    /// tile as a stop-blocker — identical behaviour to the pre-lift era,
    /// just resolved from one source of truth.
    #[test]
    fn corpse_tile_is_rejected_as_stop() {
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let corpse_pos = hex_from_offset(4, 3);
        let corpse = UnitBuilder::new(2, Team::Player, corpse_pos).hp(0).build();
        let s = snap(vec![actor.clone(), corpse]);
        let reach = reach_from(&s, &actor);
        assert!(
            !reach.destinations.contains(&corpse_pos),
            "corpse tile must not be a stop destination",
        );
    }

    /// Symmetrical invariant: a corpse is **pass-through-friendly** because
    /// it isn't a live enemy. Pathing to a tile beyond the corpse succeeds.
    #[test]
    fn tile_beyond_corpse_is_reachable_via_pass_through() {
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let corpse_pos = hex_from_offset(4, 3);
        let corpse = UnitBuilder::new(2, Team::Player, corpse_pos).hp(0).build();
        let s = snap(vec![actor.clone(), corpse]);
        let reach = reach_from(&s, &actor);
        let beyond = hex_from_offset(5, 3);
        assert!(
            reach.destinations.contains(&beyond),
            "corpse blocks stopping but lets the actor walk over",
        );
    }
}
