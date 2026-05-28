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

### Helpers: is_passable / can_stop_on (game/hex.rs)
Единые boolean-хелперы для callsite'ов BFS:
- `is_passable(cell, blockers: &HashSet<Hex>)` — `in_bounds(cell) && !blockers.contains(cell)`. Правило «что такое blocker» решает caller (враг для игрока, союзник-противник для AI threat map).
- `can_stop_on(cell, occupants: &HashSet<Hex>, self_pos: Option<Hex>)` — клетка свободна, либо это собственная стартовая клетка юнита.

## Movement Rules

- Allies are passable (can walk through, cannot stop on)
- Enemies block movement
- **Static obstacles (`blocked_hexes`) block movement *and* line-of-sight** — see below
- Speed component defines max steps per turn
- BonusMovement adds to Speed (from GrantMovement abilities); removed after move

### Static obstacles (`blocked_hexes`) — Wave 1 ch2

Statically-defined hexes occupied by terrain features (walls, crates, debris) that:

- **Block movement** — `MovementEnv.blocked_hexes` is fed into the `is_passable` and `can_stop_on` closures of `reach_from`. Pathfinding cannot route through these hexes and cannot stop on them.
- **Block line-of-sight** — `has_los(from, to, |h| blocked_hexes.contains(&h))` is the single LOS algorithm shared by all three `ActionState` backends. Used by `check_legality` to reject ranged casts with `requires_los = true` when the line is obstructed.
- Owned by the engine — stored on `CombatState.blocked_hexes: HashSet<Hex>`. Bridge mirrors it in `CombatBlockedHexes` resource for UI/AI consumers.
- Populated at combat bootstrap from `EncounterDef.obstacles` (TOML `[[encounters.obstacles]]`). Cleared on combat restart/exit. Not mutable mid-combat in Wave 1 (no destructible terrain).
- Cost-of-step penalty for AI is **not** currently applied — obstacles are binary block/no-block. Future hazard subsystem (Wave 3) will introduce severity-based soft penalties for traversable danger hexes.

## Visual Tokens

Each combatant has a `UnitToken(Entity)` component linking to a colored circle mesh spawned in `assign_hex_positions`:
- Player: dark blue `srgb(0.12, 0.22, 0.45)`, Enemy: dark red `srgb(0.45, 0.10, 0.08)`
- Radius: `HEX_SIZE * 0.75`, z-layer 0.15 (between hex fill at 0.1 and labels at 0.2)
- `update_token_positions` syncs Transform with `HexPositions` when no `MovePath` is active
- Dead tokens are hidden (`Visibility::Hidden`)

### Movement Animation

Game state (HexPositions) updates instantly. `movement_system` pushes `PendingAnim::Movement` to `AnimationQueue` with pixel waypoints. `process_animation_queue` pops it, inserts `MovePath` component on the token. `animate_movement` lerps at 0.12s per hex step. When done, `MovePath` is removed and `combat_ready()` unblocks the pipeline.

## UI Dirty Flags (Optimization)

`update_hex_visuals` caches range and move cell sets in `Local<HashSet>`. Recomputation (BFS for move, distance loop for range) only occurs when `OVERLAY` flag is set by `ui_dirty_bridge`. Cell colors update on `HEX_FILL`, labels on `LABELS`. Without dirty flags, BFS ran every frame (~60 fps); now only on actual state changes.

`HexPositions` exposes a `generation: u64` counter (incremented on insert/remove/clear) for precise change detection without false positives from `ResMut` access.
