# Test Suite Baseline (pre-refactor)

Generated: 2026-05-23
Tooling: cargo-llvm-cov 0.8.7, cargo-nextest 0.9.136, cargo-mutants 27.0.0

## Volume

| Layer | Tests | LOC |
|---|---|---|
| `src/lib.rs` (unit) | 822 | inline |
| `tests/combat_engine/*` (13 files) | 180 | ~8 000 |
| `tests/combat/*` (7 files) | 44 + 1 ignored | ~2 800 |
| `crates/combat_engine/tests/*` | ~50 | ~1 300 |
| Other (`golden_smoke`, `projection_isolation`, …) | 6 | ~500 |
| **Total** | **1184 run, 1 skipped** | ~14 600 (integration only) |

## Coverage (cargo llvm-cov --workspace)

**TOTAL: 79.25% lines / 76.56% functions / 76.83% regions** (67 387 / 13 986 missed)

### Pilot targets (already high)
| Module | Lines | Functions |
|---|---|---|
| `combat/ai/world/tags/classify.rs` | **99.42%** | 95.74% |
| `combat/ai/pipeline/stages/critics/blindspot_ranged.rs` | 95.83% | 76.47% |
| `combat/ai/pipeline/stages/critics/buff_into_void.rs` | 95.69% | 88.89% |
| `combat/ai/pipeline/stages/critics/heal_without_rescue_value.rs` | 98.54% | 87.50% |
| `combat/ai/pipeline/stages/critics/overcommit_into_danger.rs` | 97.44% | 85.71% |
| `combat/ai/pipeline/stages/critics/rare_resource_for_low_impact.rs` | 98.79% | 90.91% |
| `combat/ai/pipeline/stages/critics/self_lethal_without_payoff.rs` | 94.95% | 84.62% |
| `combat/ai/pipeline/stages/critics/mod.rs` | 96.63% | 82.76% |
| `crates/combat_engine/src/effect.rs` | 92.34% | 100.00% |

### Conclusion
Pilot targets already at **94-99% line coverage**. Refactor goal: keep ≥ baseline. Acceptance: post-refactor coverage delta ≥ -0.5pp per file.

## Performance

### Compile time (cold, after `cargo clean`)
`cargo test --no-run`: **7:10 wallclock** (CPU 3245s, 782% utilization on M-series)

### Test runtime (nextest, parallel)
**Full suite: 3.098s wallclock for 1184 tests.** Median ~10ms. No perf problem.

### Slow tests (top 10)
| Time | Test |
|---|---|
| 1.55s | `combat::replay_assert::any_of_decision_kind_passes` |
| 1.54s | `combat::replay_assert::correct_cast_ability_passes` |
| 1.53s | `combat::replay_assert::empty_overlay_exit_0` |
| 1.53s | `combat::replay_assert::correct_decision_kind_passes` |
| 1.52s | `combat::replay_assert::two_variants_or_logic_passes` |
| 1.52s | `combat::replay_assert::wrong_cast_ability_exit_1` |
| 1.52s | `combat::replay_assert::verbose_flag_prints_details` |
| 1.52s | `combat::replay_assert::missing_overlay_exit_2` |
| 1.51s | `combat::replay_assert::wrong_decision_kind_exit_1` |
| 0.70s | `golden_smoke::golden_baseline_zero_diff` |

All 9 `replay_assert::*` tests spawn `cargo run --bin replay_ai_log` — that's the per-test overhead. Engine/effect tests are 10-17ms each.

## Initial conclusions

1. **Speed is NOT a problem.** 1184 tests in 3.1s wallclock parallel is already excellent.
2. **Coverage is high** (79% workspace, 94-99% for pilot targets). Refactor risk is low IF we verify post-refactor delta.
3. **The "too many tests" smell is about LOC bloat and duplication, not runtime.** Parametrization saves source-code size and review burden, not CI time.
4. **`replay_assert::*` is the only slow cluster (9 × 1.5s = ~14s of work serialized)**. Each spawns a `cargo run --bin`. Out of scope for current refactor, but flagged.
5. **`rstest` impact on compile time** needs verification on pilot — could regress the 7:10 cold build.

## Mutation Testing (cargo mutants on critics/)

**149 mutants tested: 52 caught / 66 missed / 31 unviable → mutation score 44%**

Despite 94-99% line coverage, **less than half of arithmetic/comparison mutations are detected**. Critics tests cover lines but not boundary conditions.

| Critic | Missed | Caught | Mut.score |
|---|---|---|---|
| self_lethal_without_payoff | 28 | 12 | **30%** 🔴 |
| heal_without_rescue_value | 11 | 5 | **31%** 🔴 |
| overcommit_into_danger | 12 | 9 | 43% 🟡 |
| rare_resource_for_low_impact | 8 | 13 | 62% 🟡 |
| buff_into_void | 3 | 5 | 63% 🟡 |
| blindspot_ranged | 2 | 6 | **75%** 🟢 |

Common missed mutations:
- `replace > with == / < / >=` (heal:84, overcommit:69)
- `replace && with ||` (buff_into_void:84, heal:86)
- `replace * with + / /` (overcommit:56,71; heal:107)
- `replace - with +` (self_lethal:104; heal:106)
- `replace name() -> "" or "xyzzy"` (all critics — name() not asserted)

Full lists: `measurements/mutants-{missed,caught}-before.txt`.

## Phase 1 result (harness migration)

LOC delta: 6 critic files **−319 lines (−17%)**, test_helpers.rs **+262** → net **−57 LOC**.
Coverage delta: critics 94-98% (was 94-99%; −0.5 to −2.3pp from helper-code instrumentation, not test logic).
Production-code coverage unchanged.

## Next steps

- Phase 1.5: reinforce tests for missed mutations. Target: critics mutation score ≥70%.
- Phase 2: bridge harness extraction (`bridge_smoke.rs` setup boilerplate).
- Phase 3: parametrize `world::tags::classify` (~27 fn — uniform table-driven).
