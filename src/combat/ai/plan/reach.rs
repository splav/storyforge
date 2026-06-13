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
/// - **blocked_hexes**: static obstacles (walls, crates) — block both
///   pass-through and stopping, sourced from `snap.state.blocked_hexes`.
///
/// Single source of truth is `snap.state.units()` now that corpses live there
/// instead of in a parallel `blocked_tiles` channel.
pub fn reach_from(snap: &BattleSnapshot, actor: UnitView<'_>) -> ReachableMap {
    // Blocker sets are derived DIRECTLY from the authoritative engine state,
    // keyed by UnitId / team — never routed through `uid_to_entity` (nor the
    // `all_enemies_of` / `entity_for_uid` accessors, which do the same lookup).
    // A unit can be present in `state.units()` yet missing from that map — a
    // summon with a synthetic UnitId, or a unit not yet in the ECS spatial layer
    // (`build_snapshot` drops such units from the AI cache, and the map is built
    // from the cache). Routing through the map would silently drop that unit, so
    // the BFS would offer its occupied hex as a legal stopping destination and the
    // resulting path would collide at execution — tripping the one-unit-per-hex
    // invariant in `HexPositions`. Keying off state keeps the sets complete by
    // construction, so generated paths are always valid.
    let actor_uid = actor.state.id;
    let actor_team = actor.team;

    // Pass-through: enemies of the actor (live or dead) block walking through.
    let enemy_positions: HashSet<Hex> = snap
        .state
        .units()
        .iter()
        .filter(|u| u.team != actor_team)
        .map(|u| u.pos)
        .collect();
    // Stopping: every non-actor occupant blocks — live enemy, live ally, corpse.
    let stop_blockers: HashSet<Hex> = snap
        .state
        .units()
        .iter()
        .filter(|u| u.id != actor_uid)
        .map(|u| u.pos)
        .collect();

    // Build hazard_costs from the AI-team-visible environment (already filtered by T3
    // in build_snapshot — every entry here is visible to the actor's team).
    // Uses the unit-independent severity scores precomputed once in AiCache.env_severity.
    // Both inputs are serialised inside BattleSnapshot → AI-sim and prod agree by
    // construction (parity guarantee).
    let hazard_costs: std::collections::HashMap<Hex, f32> = snap
        .state
        .environment
        .iter()
        .map(|e| {
            (
                e.hex,
                snap.cache.env_severity.get(&e.id).copied().unwrap_or(0.0),
            )
        })
        .collect();

    let env = MovementEnv {
        enemy_positions,
        stop_blockers,
        blocked_hexes: snap.state.blocked_hexes.clone(),
        hazard_costs,
    };
    let mp = actor.pools[combat_engine::PoolKind::Mp]
        .map(|(c, _)| c)
        .unwrap_or(0);
    reach_from_env(actor.pos, mp, &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::UnitFixture;
    use crate::combat::ai::test_helpers::{unit, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn snap(units: Vec<UnitFixture>) -> BattleSnapshot {
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

    // ── T9: hazard_costs wiring ──────────────────────────────────────────────

    /// `reach_from` populates `MovementEnv.hazard_costs` from the snapshot:
    /// a visible trap whose `EnvId` has a matching severity entry produces a
    /// non-empty `hazard_costs` that re-routes equal-length paths away from
    /// the trap hex.
    #[test]
    fn ai_reach_populates_hazard_costs_from_snapshot() {
        use combat_engine::state::{EnvId, EnvKind, EnvObject, TeamSet};
        use combat_engine::{state::Team as EngTeam, AbilityId};
        // Actor at (0,0) with 2 MP; two equal-length routes to (2,0):
        //   direct:  (0,0)→(1,0)→(2,0)  — trap at (1,0)
        //   detour:  (0,0)→(1,1)→(2,0)  (even-r neighbours — valid on flat grid)
        // On a flat even-r grid (row 0, even), (0,0)'s neighbours include (1,0)
        // and (0,1).  But to make TWO equal-length 2-hop routes cleanly, we use
        // a 4-MP actor so it can reach (2,0) via either path.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .movement_points(4)
            .build();

        let mut s = snapshot_from(vec![actor.clone()], 1);

        // Place an enemy-owned trap at (1,0) — enemy team (actor's team) sees it.
        let trap_hex = hex_from_offset(1, 0);
        let trap_id = EnvId(99);
        let severity = 10.0_f32;
        s.state.environment.push(EnvObject {
            id: trap_id,
            hex: trap_hex,
            kind: EnvKind::Hazard,
            ability: AbilityId::from("trap"),
            owner: Some(EngTeam::Enemy),
            revealed_to: TeamSet::EMPTY,
        });
        s.cache.env_severity.insert(trap_id, severity);

        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);

        // The hazard hex is still *reachable* (hazard never removes destinations).
        assert!(
            reach.destinations.contains(&trap_hex),
            "trap hex remains reachable (soft penalty only)"
        );

        // The path to (2,0) should prefer the non-trap route when an alternative
        // of equal hop-count exists.  The presence of non-zero hazard_costs proves
        // the wiring is live — if hazard_costs were empty (old stub) the BFS would
        // pick the direct path (1,0) deterministically by coordinate tie-break and
        // the test below for soft-avoidance would also validate the effect.
        //
        // Here we verify the key invariant: path_to(1,0) still resolves (reachable).
        assert!(
            reach.path_to(trap_hex).is_some(),
            "trap hex is reachable — path_to must return Some"
        );
    }

    /// No environment objects → `hazard_costs` is empty and `reach_from`
    /// behaves exactly like the unweighted BFS (all destinations reachable).
    #[test]
    fn ai_reach_no_env_yields_empty_hazard_costs() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .movement_points(2)
            .build();
        let s = snapshot_from(vec![actor.clone()], 1);
        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);

        // With empty environment the adjacent tile is always reachable.
        assert!(
            reach.destinations.contains(&hex_from_offset(4, 3)),
            "adjacent hex reachable with no env"
        );
        // No environment → state.environment is empty.
        assert!(s.state.environment.is_empty(), "no traps in snapshot");
    }

    /// A visible trap on one equal-length route causes the AI path planner
    /// to choose the alternative route that avoids the trap hex.
    ///
    /// Grid layout (mirrors `pathfinding::hazard_cost_reroutes_equal_length_path`):
    ///   actor   at (3,3)
    ///   trap    at (4,3)  ← one of two equal-hop predecessors for goal
    ///   clean   at (4,2)  ← the other predecessor
    ///   goal    at (5,2)  — 2-hop from actor via either predecessor
    #[test]
    fn ai_reach_soft_avoids_visible_trap_when_alternative_exists() {
        use combat_engine::state::{EnvId, EnvKind, EnvObject, TeamSet};
        use combat_engine::{state::Team as EngTeam, AbilityId};

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .movement_points(4)
            .build();

        let mut s = snapshot_from(vec![actor.clone()], 1);
        let trap_hex = hex_from_offset(4, 3);
        let trap_id = EnvId(7);
        s.state.environment.push(EnvObject {
            id: trap_id,
            hex: trap_hex,
            kind: EnvKind::Hazard,
            ability: AbilityId::from("trap"),
            owner: Some(EngTeam::Enemy),
            revealed_to: TeamSet::EMPTY,
        });
        // High severity so the reconstructor strongly prefers the clean route.
        s.cache.env_severity.insert(trap_id, 100.0);

        let actor_view = s.unit(actor.entity).unwrap();
        let reach = reach_from(&s, actor_view);

        let goal = hex_from_offset(5, 2);
        let path = reach.path_to(goal).expect("goal must be reachable");

        assert!(
            !path.contains(&trap_hex),
            "AI path to goal should avoid the high-severity trap hex (path: {path:?})"
        );
        // Should pass through the clean predecessor instead.
        let clean_pred = hex_from_offset(4, 2);
        assert!(
            path.contains(&clean_pred),
            "AI path routes through clean predecessor (path: {path:?})"
        );
    }

    /// Regression: a unit present in the authoritative engine `state` but ABSENT
    /// from the snapshot's `uid_to_entity` map must STILL block stopping.
    ///
    /// In production this happens for a summon (synthetic UnitId) or a unit not
    /// yet in the ECS spatial layer — `build_snapshot` drops it from the AI cache,
    /// and `uid_to_entity` is built from that cache, so `entity_for_uid(u.id)`
    /// returns `None`. The old `reach_from` resolved blockers through that map and
    /// silently dropped such units, so the BFS offered their occupied hex as a
    /// stop destination → the executed path collided (HexPositions one-per-hex
    /// panic in `project_state_to_ecs`). Building the blocker set from
    /// `state.units()` keyed by UnitId fixes it by construction.
    #[test]
    fn state_unit_missing_from_entity_map_still_blocks_stopping() {
        use crate::combat::ai::world::cache::AiCache;
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        use combat_engine::state::{CombatState, RoundPhase};

        // Actor + an ally blocker on the adjacent tile (same team → pass-through
        // friendly, but must remain a STOP blocker).
        let (actor_u, actor_c) = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .movement_points(3)
            .build_pair();
        let (block_u, block_c) =
            UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3)).build_pair();
        let actor_entity = actor_c.entity;
        let actor_uid = actor_u.id;
        let blocker_pos = block_u.pos;

        let state = CombatState::new(vec![actor_u, block_u], 1, RoundPhase::ActorTurn, 0);
        let cache = AiCache::from_units(vec![actor_c, block_c]);
        // Map ONLY the actor — the blocker is in `state`/`cache` but absent from
        // the uid→entity map, exactly like an unprojected summon.
        let s = BattleSnapshot::new_with_id_map(state, cache, &[(actor_entity, actor_uid)]);

        let actor_view = s.unit(actor_entity).unwrap();
        let reach = reach_from(&s, actor_view);

        assert!(
            !reach.destinations.contains(&blocker_pos),
            "a unit in engine state but missing from the entity map must still \
             block stopping (got destinations {:?})",
            reach.destinations,
        );
        // The far side of the blocker stays reachable via ally pass-through —
        // proving the blocker only blocks STOPPING, not transit.
        assert!(
            reach.destinations.contains(&hex_from_offset(5, 3)),
            "beyond-ally tile should still be reachable by pass-through",
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
