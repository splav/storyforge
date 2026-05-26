# Combat AI — Performance Baseline

**Date:** 2026-05-26  
**Commit:** `f4ac1e7` (feat(perf): baseline benchmarks for AI snapshot rebuild + engine step)  
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
| `snapshot_rebuild_mid_encounter` | `benches/snapshot_rebuild.rs` | **1.197 µs** | `snapshot_from()` test-helper — see note below |
| `snapshot_rebuild_production`    | `benches/snapshot_rebuild_production.rs` | **2.339 µs** | production `build_snapshot()` from live ECS |
| `step_move_3hex`                 | `benches/engine_step_baseline.rs` | **2.008 µs** | `step(Move, 3 hexes)`, no AoO |
| `step_end_turn`                  | `benches/engine_step_baseline.rs` | **0.443 µs** | `step(EndTurn)`, trivial path |

> **Note on `snapshot_rebuild_mid_encounter`:** This bench measures `snapshot_from()` in
> `src/combat/ai/test_helpers.rs`, which takes an already-constructed `Vec<UnitSnapshot>` and
> assembles a `BattleSnapshot`. This is a test helper, NOT the production code path. It is
> retained as a regression guard for the snapshot assembly logic but must NOT be used as the
> basis for a "hot or cold" verdict. See the production bench above.

Criterion reported 1 high-severe outlier (1%) for snapshot_rebuild_mid_encounter,
4 high-mild (3%) + 1 high-severe (1%) for snapshot_rebuild_production, and
5 high-mild (5%) for step_move — within normal noise.

---

## Verdict

**Production `build_snapshot` is HOT.**

Using `step_move_3hex` as the representative "cost of one AI-tick action":

```
2.339 µs / 2.008 µs = 116%
```

Production `build_snapshot` takes **~16% longer than a Move step** — well above the ≥25%
threshold relative to step cost, confirming the HOT verdict.

### Call count per AI turn

`build_snapshot` is called **exactly once per AI actor turn** in `run_ai_turn()`
(`src/combat/ai/system.rs:140`). There is no beam-search or inner-loop re-invocation —
the snapshot is built once at the top of `run_ai_turn` and then reused throughout
`pick_action`, `goal_lifecycle`, and logging.

```
per_turn_cost = 2.339 µs × 1 = 2.339 µs
```

With a reference step cost of 2.008 µs, the per-turn snapshot cost exceeds the cost of
executing one Move step. For a 4-enemy encounter this translates to ~9–10 µs per round
spent on snapshot construction alone.

**Decision rule:**
- ≥ 25%  → mandate for optimization in a future session. **THIS CASE APPLIES.**
- < 25%  → benchmark stays as regression guard, no action needed.

---

## Production target re-measurement (Task A correction)

**Date:** 2026-05-26  
**Bench:** `benches/snapshot_rebuild_production.rs`  
**Commit:** see feat(perf): re-baseline benchmark for production build_snapshot

| Metric | Value |
|--------|-------|
| Production `build_snapshot` median | **2.339 µs** |
| Call count per AI turn | **1** |
| Per-turn cost | **2.339 µs** |
| Reference step cost (`step_move_3hex`) | 2.008 µs |
| Ratio to step | ~116% |
| Verdict | **HOT** (≥25% threshold exceeded) |

The earlier measurement of 1.197 µs for `snapshot_rebuild_mid_encounter` benchmarked
`snapshot_from()` — the test helper — not the production `build_snapshot()`. The
production function is ~2× more expensive because it must also: query ECS for
`AiCombatantQ` (abilities, stats, equipment), compute `estimate_st_damage` and
`estimate_damage_horizon` per unit, look up ability definitions from `ContentView`,
and build the `uid_to_entity`/`entity_to_uid` maps from `UnitIdMap`. The test helper
receives pre-computed `UnitSnapshot` rows and only assembles the final `BattleSnapshot`.

### Original deferred opportunities: applicability to production

The three opportunities noted in Task A remain applicable:

1. **`with_capacity` for HashMaps** — `uid_to_entity`/`entity_to_uid` are allocated fresh
   per call in `build_snapshot`. Pre-sizing with `HashMap::with_capacity(n)` is a
   zero-cost-to-design improvement. Still applicable.
2. **Avoid cloning `damage_horizon` and `abilities` per unit** — `build_snapshot` already
   copies `abilities` (`c.abilities.0.clone()`) and `damage_horizon` is freshly computed
   (a `Vec<f32>`). An incremental dirty-flag approach would skip both. Still applicable,
   but requires a richer caching layer.
3. **Fuse index-build passes** — `AiCache::from_units` builds its own index and
   `build_snapshot` then builds `uid_to_entity` in a second pass over the cache.
   Fusing these into one pass is still a valid opportunity.

Additionally, the production function itself has an overhead the test helper does not:
`estimate_damage_horizon` is called per unit and involves multi-ability iteration —
this is a fourth optimization candidate not present in the original task.

---

## Deferred Optimization Opportunities

Spotted while writing the bench (NOT taken — see task spec "STOP — do NOT optimize"):

1. `build_snapshot` allocates fresh `HashMap`s for `uid_to_entity`/`entity_to_uid` per call. Pre-size with `HashMap::with_capacity(n)`.
2. `abilities` (`Vec<AbilityId>`) is cloned per unit (`c.abilities.0.clone()`). If snapshots were rebuilt incrementally (dirty-flag per unit) most of these clones would be skippable.
3. `AiCache::from_units` builds its own internal index; fusing that build with the `uid_to_entity` construction would halve index-build passes.
4. `estimate_damage_horizon` is called per-unit per snapshot rebuild — it iterates all abilities and computes expected damage projections. This is unique to the production function and is likely the single largest contributor to the ~2× cost over the test helper.

These are candidates for a dedicated optimization task.

---

## Re-running

```bash
cargo bench --bench snapshot_rebuild_production   # snapshot_rebuild_production (production ECS path)
cargo bench --bench snapshot_rebuild              # snapshot_rebuild_mid_encounter (test-helper regression guard)
cargo bench --bench engine_step_baseline          # step_move_3hex, step_end_turn
```

Compare new numbers against medians above. A regression is anything more than 20% above the baseline median (criterion's own noise floor is ≈5%).
