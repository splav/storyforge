# Combat AI — Performance Baseline

**Date:** 2026-05-26  
**Commit:** this commit (benchmarks added in `feat(perf): baseline benchmarks for AI snapshot rebuild + engine step`)  
**Machine:** Darwin 25.5.0, Apple Silicon (8 cores)  
**Build:** `cargo bench` — release profile, no `--features dev`  
**Dice:** `ExpectedValue` (deterministic, no RNG variance in timing)

---

## Scenario Shape

Both benches use the same 6-unit mid-encounter:

| # | Team   | Entity raw | Position (col,row) | HP      | Notes         |
|---|--------|------------|--------------------|---------|---------------|
| 1 | Player | 1          | (2, 3)             | 35/35   | melee bruiser |
| 2 | Player | 2          | (3, 3)             | 18/30   | ranged, wounded |
| 3 | Enemy  | 10         | (7, 3)             | 25/25   | AoO-capable   |
| 4 | Enemy  | 11         | (8, 4)             | 25/25   | AoO-capable   |
| 5 | Enemy  | 12         | (5, 2)             | 0/20    | corpse (dead unit kept in snapshot) |
| 6 | Enemy  | 13         | (9, 5)             | 30/30   | ranged        |

Grid: 12×10 hexes (even-r offset coordinates).  
No statuses applied (tag cache builds trivially).  
One corpse tests that dead-unit filtering path is exercised.

---

## Bench Results (Criterion median, 100 samples)

| Bench | File | Median | Note |
|-------|------|--------|------|
| `snapshot_rebuild_mid_encounter` | `benches/snapshot_rebuild.rs` | **1.197 µs** | full `snapshot_from()` call |
| `step_move_3hex`                 | `benches/engine_step_baseline.rs` | **2.008 µs** | `step(Move, 3 hexes)`, no AoO |
| `step_end_turn`                  | `benches/engine_step_baseline.rs` | **0.443 µs** | `step(EndTurn)`, trivial path |

Criterion reported 1 high-severe outlier (1%) for snapshot rebuild and 5 high-mild (5%) for step_move — within normal noise.

---

## Verdict

**Snapshot rebuild is HOT.**

Using `step_move_3hex` as the representative "cost of one AI-tick action":

```
1.197 µs / 2.008 µs = 59.6%
```

Snapshot rebuild takes roughly **60% as long as executing a Move step** — well above the ≥25% threshold for mandatory optimization. In an AI tick that evaluates many candidate actions, `snapshot_from()` is called once per tick while `step()` may be called dozens of times (once per candidate in the sim planner). However, the absolute time is still sub-2µs; profiling the full AI tick call-count is needed to confirm total impact.

**Decision rule:**
- ≥ 25%  → mandate for optimization in a future session. **THIS CASE APPLIES.**
- < 25%  → benchmark stays as regression guard, no action needed.

---

## Deferred Optimization Opportunities

Spotted while writing the bench (NOT taken — see task spec "STOP — do NOT optimize"):

1. `snapshot_from` allocates a fresh `HashMap` for `uid_to_entity` / `entity_to_uid` on every call. For a known-size 6-unit scenario these could be pre-sized with `HashMap::with_capacity(n)` or replaced with a small flat array.
2. `unit_snapshot_to_pair` clones `damage_horizon` (a `Vec<f32>`) and `abilities` (`Vec<AbilityId>`) per unit. If snapshots were rebuilt incrementally (dirty-flag per unit) most of these clones would be skippable.
3. `AiCache::from_units` likely builds its own internal index (entity → index). Fusing that build with the `uid_to_entity` construction would halve index-build passes.

These are candidates for a dedicated optimization task.

---

## Re-running

```bash
cargo bench --bench snapshot_rebuild       # snapshot_rebuild_mid_encounter
cargo bench --bench engine_step_baseline   # step_move_3hex, step_end_turn
```

Compare new numbers against medians above. A regression is anything more than 20% above the baseline median (criterion's own noise floor is ≈5%).
