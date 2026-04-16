use std::collections::{HashMap, HashSet, VecDeque};

use hexx::Hex;

use super::hex::in_bounds;

/// BFS shortest path on the hex grid.
///
/// `is_passable(hex)` must return `true` for cells that can be entered
/// (in bounds, not occupied by an enemy). Allies count as passable.
///
/// Returns `None` if goal is unreachable.
/// Returns `Some(path)` — cells from start (exclusive) to goal (inclusive).
pub fn find_path(
    start: Hex,
    goal: Hex,
    is_passable: impl Fn(Hex) -> bool,
) -> Option<Vec<Hex>> {
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

fn reconstruct(
    start: Hex,
    goal: Hex,
    came_from: &HashMap<Hex, Hex>,
) -> Vec<Hex> {
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
}
