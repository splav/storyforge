use std::collections::{HashMap, HashSet, VecDeque};

use hexx::Hex;

use super::hex::{can_stop_on, in_bounds, is_passable};

/// BFS shortest path on the hex grid.
///
/// `is_passable(hex)` must return `true` for cells that can be entered
/// (in bounds, not occupied by an enemy). Allies count as passable.
///
/// Returns `None` if goal is unreachable.
/// Returns `Some(path)` — cells from start (exclusive) to goal (inclusive).
pub fn find_path(start: Hex, goal: Hex, is_passable: impl Fn(Hex) -> bool) -> Option<Vec<Hex>> {
    if start == goal {
        return Some(vec![]);
    }

    let mut visited: HashSet<Hex> = HashSet::new();
    let mut came_from: HashMap<Hex, Hex> = HashMap::new();
    let mut queue: VecDeque<Hex> = VecDeque::new();

    visited.insert(start);
    queue.push_back(start);

    while let Some(current) = queue.pop_front() {
        for nb in current.all_neighbors() {
            if visited.contains(&nb) {
                continue;
            }
            visited.insert(nb);

            if !is_passable(nb) {
                continue;
            }

            came_from.insert(nb, current);

            if nb == goal {
                return Some(reconstruct(start, goal, &came_from));
            }

            queue.push_back(nb);
        }
    }

    None
}

fn reconstruct(start: Hex, goal: Hex, came_from: &HashMap<Hex, Hex>) -> Vec<Hex> {
    let mut path = vec![goal];
    let mut cur = goal;
    while let Some(&prev) = came_from.get(&cur) {
        if prev == start {
            break;
        }
        path.push(prev);
        cur = prev;
    }
    path.reverse();
    path
}

/// BFS flood fill: all cells reachable from `start` in up to `max_steps` hex steps.
///
/// `is_passable(hex)` — can the unit pass through this cell (empty or ally)?
/// `can_stop(hex)` — can the unit end its move here (must be empty)?
///
/// Returns the set of valid destinations (excludes `start`).
pub fn reachable_cells(
    start: Hex,
    max_steps: i32,
    is_passable: impl Fn(Hex) -> bool,
    can_stop: impl Fn(Hex) -> bool,
) -> HashSet<Hex> {
    reachable_with_paths(start, max_steps, is_passable, can_stop).destinations
}

/// Same as `reachable_cells` but also stores `came_from` so paths can be reconstructed
/// via `ReachableMap::path_to` without a second BFS.
pub fn reachable_with_paths(
    start: Hex,
    max_steps: i32,
    is_passable: impl Fn(Hex) -> bool,
    can_stop: impl Fn(Hex) -> bool,
) -> ReachableMap {
    let mut visited: HashSet<Hex> = HashSet::new();
    let mut destinations: HashSet<Hex> = HashSet::new();
    let mut came_from: HashMap<Hex, Hex> = HashMap::new();
    let mut queue: VecDeque<(Hex, i32)> = VecDeque::new();

    visited.insert(start);
    queue.push_back((start, 0));

    while let Some((current, dist)) = queue.pop_front() {
        if dist >= max_steps {
            continue;
        }
        for nb in current.all_neighbors() {
            if visited.contains(&nb) {
                continue;
            }
            if !in_bounds(nb) || !is_passable(nb) {
                continue;
            }
            visited.insert(nb);
            came_from.insert(nb, current);
            if can_stop(nb) {
                destinations.insert(nb);
            }
            queue.push_back((nb, dist + 1));
        }
    }

    ReachableMap {
        start,
        destinations,
        came_from,
    }
}

pub struct ReachableMap {
    pub start: Hex,
    pub destinations: HashSet<Hex>,
    came_from: HashMap<Hex, Hex>,
}

/// Movement environment for `reach_from` — the tile-sets that shape the BFS.
/// Both AI (snapshot-backed) and UI (Bevy-backed) call the same pathfinding
/// core with this struct; only env construction differs.
///
/// - `enemy_positions` — cells the actor cannot **pass through** (alive enemy
///   occupants). Allies are absent here: allies block stopping but allow
///   pass-through.
/// - `stop_blockers` — cells the actor cannot **stop on**. Every non-actor
///   occupant (enemy or ally) plus environmental blockers (corpses, reserved
///   tiles for the AI side, etc.).
/// - `blocked_hexes` — static obstacles. Blocks **both** pass-through and
///   stopping. Populated from `CombatState.blocked_hexes` (walls, crates, …).
///   Empty by default (no obstacles in ch1 scenarios).
/// - `hazard_costs` — per-tile soft movement penalties (e.g. known traps).
///   When empty (the default) `reach_from` is byte-identical to the plain BFS
///   — reachability, `destinations`, and every `path_to` result are unchanged.
///   When non-empty the reachable *set* is still governed by unweighted
///   hop-count (hazards never add/remove tiles), but `path_to` returns the
///   min-penalty path among all equal-length shortest paths.  Tie-break between
///   equal-penalty predecessors: lexicographic `(hex.x, hex.y)` — never
///   HashMap-iteration order — so `path_to` is deterministic across runs and
///   replay-stable. Populated by T9; UI always leaves it empty.
pub struct MovementEnv {
    pub enemy_positions: HashSet<Hex>,
    pub stop_blockers: HashSet<Hex>,
    /// Static obstacles — blocks both pass-through and stopping.
    pub blocked_hexes: HashSet<Hex>,
    /// Soft per-tile hazard penalties. Empty = today's plain BFS behaviour.
    pub hazard_costs: HashMap<Hex, f32>,
}

/// BFS reach from `start` using a prepared `MovementEnv`. Thin wrapper over
/// `reachable_with_paths` that wires the env's sets into `is_passable` /
/// `can_stop_on`. Keeps the BFS closures in one place so a future change to
/// movement rules (e.g. difficult terrain) lands once.
///
/// `blocked_hexes` blocks both pass-through and stopping (static obstacles).
///
/// When `env.hazard_costs` is empty this function is byte-identical to the
/// plain BFS (replay-stable). When non-empty the reachable set is unchanged but
/// the `came_from` predecessor map is recomputed to minimise accumulated hazard
/// penalty along equal-length shortest paths (see [`reweight_came_from`]).
pub fn reach_from(start: Hex, max_steps: i32, env: &MovementEnv) -> ReachableMap {
    let mut map = reachable_with_paths(
        start,
        max_steps,
        |h| !env.blocked_hexes.contains(&h) && is_passable(h, &env.enemy_positions),
        |h| !env.blocked_hexes.contains(&h) && can_stop_on(h, &env.stop_blockers, None),
    );

    if !env.hazard_costs.is_empty() {
        reweight_came_from(&mut map, &env.hazard_costs, max_steps, |h| {
            !env.blocked_hexes.contains(&h) && is_passable(h, &env.enemy_positions)
        });
    }

    map
}

/// Recompute `came_from` in `map` to prefer minimum accumulated hazard penalty
/// while keeping hop-count distances identical (reachability unchanged).
///
/// Algorithm:
/// 1. Recover BFS distances via second BFS flood (same passable predicate).
/// 2. Process nodes in order of increasing distance.
/// 3. For each node, among its neighbours at distance `d-1` (valid shortest-
///    path predecessors), pick the one minimising `best_penalty[pred] +
///    hazard_costs.get(node)`.  Tie-break by `(hex.x, hex.y)` lexicographic
///    order — never HashMap iteration order — to guarantee determinism.
fn reweight_came_from(
    map: &mut ReachableMap,
    hazard_costs: &HashMap<Hex, f32>,
    max_steps: i32,
    is_passable: impl Fn(Hex) -> bool,
) {
    // --- Step 1: recover per-node BFS distances --------------------------------
    let mut dist: HashMap<Hex, i32> = HashMap::new();
    {
        let mut queue: VecDeque<(Hex, i32)> = VecDeque::new();
        dist.insert(map.start, 0);
        queue.push_back((map.start, 0));
        while let Some((cur, d)) = queue.pop_front() {
            if d >= max_steps {
                continue;
            }
            let mut neighbours: Vec<Hex> = cur.all_neighbors().to_vec();
            neighbours.sort_unstable_by_key(|h| (h.x, h.y));
            for nb in neighbours {
                if dist.contains_key(&nb) {
                    continue;
                }
                if !in_bounds(nb) || !is_passable(nb) {
                    continue;
                }
                dist.insert(nb, d + 1);
                queue.push_back((nb, d + 1));
            }
        }
    }

    // Collect all reachable nodes (excluding start) sorted by distance so we
    // process predecessors before their successors.
    let mut nodes_by_dist: Vec<(i32, Hex)> = dist
        .iter()
        .filter(|(&h, _)| h != map.start)
        .map(|(&h, &d)| (d, h))
        .collect();
    // Primary sort: distance; secondary: deterministic (x,y) to make the
    // processing order stable even though it doesn't affect correctness here.
    nodes_by_dist.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| (a.1.x, a.1.y).cmp(&(b.1.x, b.1.y)))
    });

    // --- Step 2: compute min-penalty predecessor for each node ----------------
    // best_penalty[h] = minimum accumulated hazard penalty on any shortest path
    // from start to h.
    let mut best_penalty: HashMap<Hex, f32> = HashMap::new();
    best_penalty.insert(map.start, 0.0);

    let mut new_came_from: HashMap<Hex, Hex> = HashMap::new();

    for (d, node) in &nodes_by_dist {
        let node_cost = hazard_costs.get(node).copied().unwrap_or(0.0);

        // Candidate predecessors: neighbours at distance d-1.
        let mut candidates: Vec<Hex> = node.all_neighbors().to_vec();
        // Sort deterministically so tie-breaking is by (x,y), not HashMap order.
        candidates.sort_unstable_by_key(|h| (h.x, h.y));

        let best_pred = candidates
            .into_iter()
            .filter(|pred| dist.get(pred).copied() == Some(d - 1))
            .min_by(|a, b| {
                let pa = best_penalty.get(a).copied().unwrap_or(f32::INFINITY) + node_cost;
                let pb = best_penalty.get(b).copied().unwrap_or(f32::INFINITY) + node_cost;
                pa.partial_cmp(&pb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Tie-break: lexicographic (x, y) — deterministic, not iteration order.
                    .then_with(|| (a.x, a.y).cmp(&(b.x, b.y)))
            });

        if let Some(pred) = best_pred {
            let pen = best_penalty.get(&pred).copied().unwrap_or(f32::INFINITY) + node_cost;
            best_penalty.insert(*node, pen);
            new_came_from.insert(*node, pred);
        }
    }

    map.came_from = new_came_from;
}

impl ReachableMap {
    /// Reconstruct the path from start to `goal`. Returns start-exclusive, goal-inclusive.
    /// Returns `None` if `goal` is not in the BFS tree.
    pub fn path_to(&self, goal: Hex) -> Option<Vec<Hex>> {
        if !self.came_from.contains_key(&goal) {
            return None;
        }
        let mut path = vec![goal];
        let mut cur = goal;
        while let Some(&prev) = self.came_from.get(&cur) {
            if prev == self.start {
                break;
            }
            path.push(prev);
            cur = prev;
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::hex::in_bounds;

    fn passable(blocked: &[Hex]) -> impl Fn(Hex) -> bool + '_ {
        |h| in_bounds(h) && !blocked.contains(&h)
    }

    use crate::game::hex::hex_from_offset;

    #[test]
    fn same_cell_returns_empty_path() {
        let c = hex_from_offset(1, 1);
        let path = find_path(c, c, passable(&[]));
        assert_eq!(path, Some(vec![]));
    }

    #[test]
    fn direct_neighbor_path_length_one() {
        let a = hex_from_offset(1, 0);
        let b = hex_from_offset(2, 0);
        let path = find_path(a, b, passable(&[])).unwrap();
        assert_eq!(path, vec![b]);
    }

    #[test]
    fn blocked_by_enemy_returns_none() {
        let c = hex_from_offset(1, 1);
        let all_blocked: Vec<Hex> = c.all_neighbors().to_vec();
        let path = find_path(c, hex_from_offset(3, 1), |h| {
            in_bounds(h) && !all_blocked.contains(&h)
        });
        assert_eq!(path, None);
    }

    #[test]
    fn path_avoids_enemy() {
        let blocked = hex_from_offset(2, 0);
        let a = hex_from_offset(1, 0);
        let b = hex_from_offset(3, 0);
        let path = find_path(a, b, passable(&[blocked])).unwrap();
        assert!(!path.contains(&blocked));
        assert_eq!(*path.last().unwrap(), b);
    }

    #[test]
    fn ally_is_passable() {
        let a = hex_from_offset(1, 0);
        let b = hex_from_offset(3, 0);
        let path = find_path(a, b, passable(&[])).unwrap();
        assert_eq!(*path.last().unwrap(), b);
    }

    #[test]
    fn out_of_bounds_goal_returns_none() {
        let a = hex_from_offset(0, 0);
        let b = hex_from_offset(99, 99);
        let path = find_path(a, b, passable(&[]));
        assert_eq!(path, None);
    }

    /// Pin the two env-set semantics at the pathfinding layer: `enemy_positions`
    /// must block *pass-through* (so tiles behind the enemy stay unreachable),
    /// while `stop_blockers` only blocks *stopping* (tiles beyond the blocker
    /// remain reachable via walk-through — the ally case).
    #[test]
    fn reach_from_separates_pass_through_from_stop_rules() {
        use std::collections::HashSet;
        let start = hex_from_offset(3, 3);

        // Case 1 — ally (stop blocker, not pass-through blocker). Tile at
        // ally position is excluded from destinations, but tiles past the
        // ally are still reachable.
        let ally_tile = hex_from_offset(4, 3);
        let beyond_ally = hex_from_offset(5, 3);
        let mut stop_only = HashSet::new();
        stop_only.insert(ally_tile);
        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: stop_only,
            blocked_hexes: HashSet::new(),
            hazard_costs: HashMap::new(),
        };
        let reach = reach_from(start, 3, &env);
        assert!(
            !reach.destinations.contains(&ally_tile),
            "cannot stop on ally"
        );
        assert!(
            reach.destinations.contains(&beyond_ally),
            "tiles past an ally must still be reachable (pass-through allowed)",
        );

        // Case 2 — enemy (pass-through blocker). Tile at enemy is excluded
        // AND tiles strictly behind the enemy on the same axis become
        // unreachable within 1 MP of the enemy.
        let enemy_tile = hex_from_offset(4, 3);
        let behind_enemy = hex_from_offset(5, 3);
        let mut enemies = HashSet::new();
        enemies.insert(enemy_tile);
        let mut blockers = HashSet::new();
        blockers.insert(enemy_tile);
        let env2 = MovementEnv {
            enemy_positions: enemies,
            stop_blockers: blockers,
            blocked_hexes: HashSet::new(),
            hazard_costs: HashMap::new(),
        };
        let reach2 = reach_from(start, 2, &env2);
        assert!(!reach2.destinations.contains(&enemy_tile));
        assert!(
            !reach2.destinations.contains(&behind_enemy),
            "enemy blocks pass-through, so tiles straight behind stay out of reach at MP=2",
        );
    }

    // ── blocked_hexes tests (T1.2.2) ─────────────────────────────────────────

    /// A hex in `blocked_hexes` cannot be passed through or stopped on.
    /// With MP=1 the only forward step is blocked, so the tile behind it stays
    /// unreachable. We also verify the obstacle itself is excluded.
    #[test]
    fn obstacle_blocks_movement_through() {
        use std::collections::HashSet;
        let start = hex_from_offset(3, 3);
        let obstacle = hex_from_offset(4, 3);
        // With MP=1 the BFS can reach only neighbors of start; obstacle is 1
        // step away and is blocked, so it must not appear in destinations.
        let mut blocked = HashSet::new();
        blocked.insert(obstacle);
        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: blocked,
            hazard_costs: HashMap::new(),
        };
        let reach_mp1 = reach_from(start, 1, &env);
        assert!(
            !reach_mp1.destinations.contains(&obstacle),
            "obstacle tile must not be a destination (pass-through blocked)",
        );
        // With a full row of obstacles the BFS cannot get past them.
        // Place obstacles at all 6 neighbors of start — nothing reachable.
        let mut all_blocked: HashSet<Hex> = HashSet::new();
        for nb in start.all_neighbors() {
            all_blocked.insert(nb);
        }
        let env_all = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: all_blocked,
            hazard_costs: HashMap::new(),
        };
        let reach_all = reach_from(start, 3, &env_all);
        assert!(
            reach_all.destinations.is_empty(),
            "when all neighbors are blocked nothing must be reachable",
        );
    }

    /// A hex in `blocked_hexes` cannot be stopped on (even without enemy/ally).
    #[test]
    fn obstacle_blocks_stopping_on() {
        use std::collections::HashSet;
        let start = hex_from_offset(3, 3);
        let obstacle = hex_from_offset(4, 3);
        let mut blocked = HashSet::new();
        blocked.insert(obstacle);
        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: blocked,
            hazard_costs: HashMap::new(),
        };
        let reach = reach_from(start, 2, &env);
        assert!(
            !reach.destinations.contains(&obstacle),
            "obstacle must not appear in destinations (cannot stop on it)",
        );
    }

    /// An obstacle on one hex does not prevent reaching a neighbor on a different
    /// path (non-collinear route). The BFS routes around it if MP allows.
    #[test]
    fn obstacle_does_not_block_diagonal_path() {
        use std::collections::HashSet;
        // Grid layout (even-r offset): start at (3,3). Obstacle at (4,3).
        // The neighbor at (4,2) is reachable directly without passing through (4,3).
        let start = hex_from_offset(3, 3);
        let obstacle = hex_from_offset(4, 3);
        let side_neighbor = hex_from_offset(4, 2); // neighbor of start, not blocked
        let mut blocked = HashSet::new();
        blocked.insert(obstacle);
        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: blocked,
            hazard_costs: HashMap::new(),
        };
        let reach = reach_from(start, 2, &env);
        assert!(
            reach.destinations.contains(&side_neighbor),
            "unblocked neighbor must still be reachable even with adjacent obstacle",
        );
    }

    /// Probe: find tiles reachable via multiple equal-hop routes.
    #[test]
    #[ignore]
    fn probe_multiple_routes() {
        let start = hex_from_offset(3, 3);
        println!("start axial: ({}, {})", start.x, start.y);
        let hop1: Vec<Hex> = start
            .all_neighbors()
            .iter()
            .copied()
            .filter(|&h| in_bounds(h))
            .collect();
        println!("hop-1 in-bounds:");
        for h in &hop1 {
            println!("  axial ({}, {})", h.x, h.y);
        }
        for h1 in &hop1 {
            for h2 in h1.all_neighbors() {
                if h2 == start {
                    continue;
                }
                if hop1.contains(&h2) {
                    continue;
                }
                if !in_bounds(h2) {
                    continue;
                }
                let other_preds: Vec<_> = hop1
                    .iter()
                    .filter(|&&p| p != *h1 && p.all_neighbors().contains(&h2))
                    .collect();
                if !other_preds.is_empty() {
                    println!(
                        "  2-hop multi-route goal ({},{}) via ({},{}) and {}",
                        h2.x,
                        h2.y,
                        h1.x,
                        h1.y,
                        other_preds
                            .iter()
                            .map(|h| format!("({},{})", h.x, h.y))
                            .collect::<Vec<_>>()
                            .join("/")
                    );
                }
            }
        }
    }

    // ── hazard_costs tests (T8) ───────────────────────────────────────────────

    /// Build a minimal MovementEnv (no enemies, no blockers) with optional hazard costs.
    fn open_env(hazard_costs: HashMap<Hex, f32>) -> MovementEnv {
        MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: HashSet::new(),
            hazard_costs,
        }
    }

    /// With empty `hazard_costs`, `reach_from` must produce byte-identical output
    /// to a direct `reachable_with_paths` call (no reweighting path is taken).
    /// Destinations and every reconstructed path must match.
    #[test]
    fn empty_hazard_costs_byte_identical_to_legacy_bfs() {
        let start = hex_from_offset(3, 3);
        let max_steps = 3;

        // Plain BFS — the reference.
        let legacy = reachable_with_paths(start, max_steps, in_bounds, in_bounds);

        // reach_from with empty hazard_costs must follow the same code path.
        let with_env = reach_from(start, max_steps, &open_env(HashMap::new()));

        assert_eq!(
            with_env.destinations, legacy.destinations,
            "destinations must be identical to plain BFS with empty hazard_costs",
        );

        // Pin path_to for a set of explicit goals so a future change is caught.
        let pinned_goals = [
            hex_from_offset(4, 3), // 1-hop
            hex_from_offset(3, 4), // 1-hop
            hex_from_offset(4, 4), // 2-hop, multi-route
            hex_from_offset(5, 2), // 2-hop, multi-route
        ];
        for goal in pinned_goals {
            assert_eq!(
                with_env.path_to(goal),
                legacy.path_to(goal),
                "path_to({goal:?}) must match legacy BFS",
            );
        }
    }

    /// When two equal-hop paths exist and one crosses a hazard tile, `path_to`
    /// must avoid the hazard hex (choose the clean path).
    ///
    /// Layout: start=(3,3). Goal axial (3,2)=offset(5,2) is 2 hops away via
    /// either (4,3) [axial (2,3)] or (4,2) [axial (2,2)]. Place a hazard cost
    /// on (4,3); the path must route through (4,2) instead.
    #[test]
    fn hazard_cost_reroutes_equal_length_path() {
        let start = hex_from_offset(3, 3);
        let hazard_hex = hex_from_offset(4, 3); // axial (2,3) — one of two predecessors
        let clean_pred = hex_from_offset(4, 2); // axial (2,2) — the other predecessor
        let goal = hex_from_offset(5, 2); // axial (3,2) — 2-hop goal

        let mut costs = HashMap::new();
        costs.insert(hazard_hex, 10.0);

        let reach = reach_from(start, 3, &open_env(costs));

        assert!(
            reach.destinations.contains(&goal),
            "goal must still be reachable"
        );

        let path = reach.path_to(goal).expect("path_to goal must exist");
        assert!(
            !path.contains(&hazard_hex),
            "path must avoid the hazard hex ({hazard_hex:?}); got {path:?}",
        );
        assert!(
            path.contains(&clean_pred),
            "path must route through the clean predecessor ({clean_pred:?}); got {path:?}",
        );
    }

    /// A goal reachable *only* through a hazard corridor must still appear in
    /// `destinations` — hazard costs are soft, not hard blockers.
    ///
    /// Construction: block all neighbors of start except one (the hazard hex),
    /// leave the cell beyond the hazard open as the goal.
    #[test]
    fn hazard_tile_still_reachable_when_only_option() {
        let start = hex_from_offset(3, 3);
        let hazard_hex = hex_from_offset(4, 3); // only open neighbour of start
        let goal = hex_from_offset(5, 3); // one step past the hazard

        // Block all neighbours of start except hazard_hex.
        let open_nbs: HashSet<Hex> = start
            .all_neighbors()
            .iter()
            .copied()
            .filter(|&h| h != hazard_hex && in_bounds(h))
            .collect();

        let mut costs = HashMap::new();
        costs.insert(hazard_hex, 999.0);

        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: HashSet::new(),
            blocked_hexes: open_nbs, // block every neighbour except the hazard one
            hazard_costs: costs,
        };
        let reach = reach_from(start, 3, &env);

        assert!(
            reach.destinations.contains(&goal),
            "goal must be reachable even though the only route passes through a hazard",
        );

        let path = reach.path_to(goal).expect("path_to goal must exist");
        assert!(
            path.contains(&hazard_hex),
            "the through-hazard path must include the hazard hex; got {path:?}",
        );
    }

    /// Adding hazard costs must not change the reachable set — `destinations`
    /// must be identical regardless of whether `hazard_costs` is empty.
    #[test]
    fn hazard_cost_does_not_expand_or_shrink_reachable_set() {
        let start = hex_from_offset(3, 3);
        let goal_area = hex_from_offset(4, 3); // a typical hazard candidate

        let mut costs = HashMap::new();
        costs.insert(goal_area, 5.0);
        costs.insert(hex_from_offset(3, 4), 3.0);

        let without_hazard = reach_from(start, 3, &open_env(HashMap::new()));
        let with_hazard = reach_from(start, 3, &open_env(costs));

        assert_eq!(
            with_hazard.destinations, without_hazard.destinations,
            "reachable set must be identical with and without hazard costs",
        );
    }

    /// When two predecessors of a node carry equal accumulated penalty, the one
    /// with lexicographically smaller axial `(x, y)` must be chosen — never
    /// HashMap iteration order.
    ///
    /// Goal axial (2,4)=offset(4,4) is 2 hops from start (3,3) via two equal-hop
    /// predecessors: axial (2,3)=offset(4,3) and axial (1,4)=offset(3,4).
    /// With no hazards on either predecessor (only a cost on the goal itself so
    /// `reweight_came_from` runs), both accumulated penalties are equal, so the
    /// tie-break must choose the lex-min predecessor: axial (1,4)=offset(3,4).
    #[test]
    fn equal_penalty_tie_is_deterministic() {
        let start = hex_from_offset(3, 3);
        let goal = hex_from_offset(4, 4); // axial (2,4)
        let lex_pred = hex_from_offset(3, 4); // axial (1,4) — lex-min predecessor
        let other_pred = hex_from_offset(4, 3); // axial (2,3) — the other predecessor

        // Put a cost only on the goal itself so both paths accumulate the same
        // penalty (the goal's cost) and the tie-break fires.
        let mut costs = HashMap::new();
        costs.insert(goal, 1.0);

        let reach = reach_from(start, 3, &open_env(costs));

        assert!(reach.destinations.contains(&goal), "goal must be reachable");

        let path = reach.path_to(goal).expect("path_to goal must exist");
        // Path is [predecessor, goal] — the predecessor at index 0 must be the lex-min one.
        assert_eq!(
            path,
            vec![lex_pred, goal],
            "tie-break must choose lex-min predecessor {lex_pred:?}, not {other_pred:?}; got {path:?}",
        );
    }

    /// Empty `blocked_hexes` does not change the movement behaviour compared to
    /// the previous env layout with only enemy/stop_blockers.
    #[test]
    fn empty_blocked_hexes_does_not_change_behavior() {
        use std::collections::HashSet;
        let start = hex_from_offset(3, 3);
        let ally_tile = hex_from_offset(4, 3);
        let beyond_ally = hex_from_offset(5, 3);
        let mut stop_only = HashSet::new();
        stop_only.insert(ally_tile);
        let env = MovementEnv {
            enemy_positions: HashSet::new(),
            stop_blockers: stop_only,
            blocked_hexes: HashSet::new(),
            hazard_costs: HashMap::new(),
        };
        let reach = reach_from(start, 3, &env);
        assert!(
            !reach.destinations.contains(&ally_tile),
            "ally tile still not stoppable with empty blocked_hexes",
        );
        assert!(
            reach.destinations.contains(&beyond_ally),
            "tile beyond ally still reachable with empty blocked_hexes",
        );
    }
}
