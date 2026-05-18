# Benches

## `engine_move`

Engine vs legacy parity bench for a 10-unit Move action. Lives alongside the
Phase 0 spike's acceptance criterion: **engine path must be ≤ 1.2× the legacy
sim path** (i.e. no worse than 20% slower).

### Phase 6 baseline (2026-05-18, after `unisim/phase6-complete`)

| Bench | Time (median) | Notes |
|---|---|---|
| `bench_move_10units_engine` | **3.964 µs** | engine `step(Action::Move)` path |
| `bench_move_10units_legacy` | **4.520 µs** | legacy `sim::apply_step(PlanStep::Move)` path |

Ratio: engine ≈ **0.88× legacy** (engine is ~14% **faster**). Comfortably
under the Phase 0 gate of 1.2×.

Baseline persisted on disk at:

```
target/criterion/bench_move_10units_engine/phase6/
target/criterion/bench_move_10units_legacy/phase6/
```

### Running

```bash
# One-shot run with median estimates printed:
cargo bench --bench engine_move

# Save the current numbers as a named baseline (replays after a change
# compare against it):
cargo bench --bench engine_move -- --save-baseline phase6

# Compare a new run against the saved baseline:
cargo bench --bench engine_move -- --baseline phase6
```

### Phase 0 baseline status

Phase 0 (the steel-thread spike, 2026-05-12) did not check a labelled
baseline into the repo — the bench produced ad-hoc numbers used to gate
the migration GO decision. The `phase6` baseline captured here is the
first persisted reference point.

Future phases that touch the engine `step()` hot path or the sim
substrate should compare against `phase6` and document the delta in
their retrospective.
