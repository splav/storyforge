# Hex Grid

## Coordinate System

**Even-r offset**: pointy-top hexagons, even rows (r & 1 == 0) shift right by 0.5.

```
Row 0 (even):   [0,0] [1,0] [2,0] [3,0] [4,0] [5,0] [6,0]          — 7 cells
Row 1 (odd):  [0,1] [1,1] [2,1] [3,1] [4,1] [5,1] [6,1] [7,1]      — 8 cells
Row 2 (even):   [0,2] [1,2] [2,2] [3,2] [4,2] [5,2] [6,2]          — 7 cells
...
```

Grid: `GRID_COLS = 8`, `GRID_ROWS = 7`. Even rows have 7 cells (0..6), odd rows have 8 cells (0..7).

## Neighbors

Even row (r & 1 == 0):
```
(q-1, r), (q+1, r), (q, r-1), (q+1, r-1), (q, r+1), (q+1, r+1)
```

Odd row (r & 1 != 0):
```
(q-1, r), (q+1, r), (q-1, r-1), (q, r-1), (q-1, r+1), (q, r+1)
```

## Distance

Conversion to cube coordinates:
```
q_cube = q - (r + (r & 1)) / 2
r_cube = r
s_cube = -q_cube - r_cube
```

Distance = `max(|dq|, |dr|, |ds|)` in cube coordinates.

## Pixel Mapping

```
shift = (r & 1 == 0) ? 0.5 : 0.0
x = HEX_SIZE * sqrt(3) * (q + shift)
y = HEX_SIZE * 1.5 * r
```

`HEX_SIZE = 34.0`. Hex mesh: `RegularPolygon(HEX_SIZE * 0.97, 6)` (pointy-top, Bevy default).

## Pathfinding

### find_path(start, goal, is_passable)
BFS shortest path. Returns `Option<Vec<(i32, i32)>>` — start-exclusive, goal-inclusive. `None` if unreachable.

### reachable_cells(start, max_steps, is_passable, can_stop)
BFS flood fill up to `max_steps`. Two predicates:
- `is_passable(q, r)` — can pass through (empty or ally cell)
- `can_stop(q, r)` — can end movement here (empty cell only)

Returns `HashSet<(i32, i32)>` of valid destinations.

## Movement Rules

- Allies are passable (can walk through, cannot stop on)
- Enemies block movement
- Speed component defines max steps per turn
- BonusMovement overrides Speed when present (from GrantMovement abilities)
