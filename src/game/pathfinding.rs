use std::collections::{HashMap, HashSet, VecDeque};

use super::hex::{hex_neighbors, in_bounds};

/// BFS shortest path on the hex grid.
///
/// `is_passable(q, r)` must return `true` for cells that can be entered
/// (in bounds, not occupied by an enemy). Allies count as passable.
///
/// Returns `None` if goal is unreachable.
/// Returns `Some(path)` — cells from start (exclusive) to goal (inclusive).
pub fn find_path(
    start: (i32, i32),
    goal: (i32, i32),
    is_passable: impl Fn(i32, i32) -> bool,
) -> Option<Vec<(i32, i32)>> {
    if start == goal {
        return Some(vec![]);
    }

    let mut visited: HashSet<(i32, i32)> = HashSet::new();
    let mut came_from: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    let mut queue: VecDeque<(i32, i32)> = VecDeque::new();

    visited.insert(start);
    queue.push_back(start);

    while let Some(current) = queue.pop_front() {
        for nb in hex_neighbors(current.0, current.1) {
            if visited.contains(&nb) {
                continue;
            }
            visited.insert(nb);

            if !is_passable(nb.0, nb.1) {
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
    start: (i32, i32),
    goal: (i32, i32),
    came_from: &HashMap<(i32, i32), (i32, i32)>,
) -> Vec<(i32, i32)> {
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
/// `is_passable(q, r)` — can the unit pass through this cell (empty or ally)?
/// `can_stop(q, r)` — can the unit end its move here (must be empty)?
///
/// Returns the set of valid destinations (excludes `start`).
pub fn reachable_cells(
    start: (i32, i32),
    max_steps: i32,
    is_passable: impl Fn(i32, i32) -> bool,
    can_stop: impl Fn(i32, i32) -> bool,
) -> HashSet<(i32, i32)> {
    let mut visited: HashSet<(i32, i32)> = HashSet::new();
    let mut result: HashSet<(i32, i32)> = HashSet::new();
    let mut queue: VecDeque<((i32, i32), i32)> = VecDeque::new();

    visited.insert(start);
    queue.push_back((start, 0));

    while let Some((current, dist)) = queue.pop_front() {
        if dist >= max_steps {
            continue;
        }
        for nb in hex_neighbors(current.0, current.1) {
            if visited.contains(&nb) {
                continue;
            }
            if !in_bounds(nb.0, nb.1) || !is_passable(nb.0, nb.1) {
                continue;
            }
            visited.insert(nb);
            if can_stop(nb.0, nb.1) {
                result.insert(nb);
            }
            queue.push_back((nb, dist + 1));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::hex::{hex_neighbors, in_bounds};

    fn passable(blocked: &[(i32, i32)]) -> impl Fn(i32, i32) -> bool + '_ {
        |q, r| in_bounds(q, r) && !blocked.contains(&(q, r))
    }

    #[test]
    fn same_cell_returns_empty_path() {
        let path = find_path((1, 1), (1, 1), passable(&[]));
        assert_eq!(path, Some(vec![]));
    }

    #[test]
    fn direct_neighbor_path_length_one() {
        // (1,0) and (2,0) are horizontal neighbors in even row.
        let path = find_path((1, 0), (2, 0), passable(&[])).unwrap();
        assert_eq!(path, vec![(2, 0)]);
    }

    #[test]
    fn blocked_by_enemy_returns_none() {
        let all_blocked: Vec<(i32, i32)> = hex_neighbors(1, 1).into_iter().collect();
        let path = find_path((1, 1), (3, 1), |q, r| {
            in_bounds(q, r) && !all_blocked.contains(&(q, r))
        });
        assert_eq!(path, None);
    }

    #[test]
    fn path_avoids_enemy() {
        // Enemy at (2,0) forces path to go around via row 1.
        let blocked = [(2, 0)];
        let path = find_path((1, 0), (3, 0), passable(&blocked)).unwrap();
        assert!(!path.contains(&(2, 0)));
        assert_eq!(*path.last().unwrap(), (3, 0));
    }

    #[test]
    fn ally_is_passable() {
        // Ally at (2,0) — path goes straight through.
        let path = find_path((1, 0), (3, 0), passable(&[])).unwrap();
        assert_eq!(*path.last().unwrap(), (3, 0));
    }

    #[test]
    fn out_of_bounds_goal_returns_none() {
        let path = find_path((0, 0), (99, 99), passable(&[]));
        assert_eq!(path, None);
    }
}
