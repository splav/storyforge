# Test Helpers Normalisation Plan

Plan to unify scattered test helpers into the layout documented in
[testing.md §3](../testing.md). One-off refactor; **not** an ongoing effort.

**Status:** draft, awaiting approval to start.

**Context:** after Phases 1-4 of the test-coverage work in mid-May 2026,
multiple test files re-invented helpers locally because the convention for
where shared scaffolding lives wasn't clear. This plan consolidates them.

---

## 1. Current state

| Location | Purpose | Consumers |
|---|---|---|
| `src/combat/ai/test_helpers.rs` (~700 LOC) | Critic harness: `CriticScenarioBuilder`, `UnitBuilder`, `StageTestHarness`, `run_critic`, `assert_critic_fires/passes`, `assert_stage_critic_fires/passes` | Inline tests in `src/combat/ai/pipeline/stages/critics/*.rs` |
| `tests/common/mod.rs` (185 LOC) | App / engine fixtures: `base_stats`, `test_equipment`, `movement_app`, `init_engine_state`, `insert_haste_status` etc. | Integration tests in `tests/combat/` and `tests/combat_engine/` |
| `tests/common/bridge.rs` (418 LOC) | Bridge-layer scaffolding: `bridge_app`, `projector_only_app`, `spawn_caster/target/_with_speed`, `spawn_enemy_with_weapon`, `bootstrap`, `MeleeContent`, `write_move/cast`, `script_no_crit_fail/d20`, `with_engine_unit`, `default_equipment` | Mostly `tests/combat_engine/bridge_smoke.rs` |
| **Inline locals** | Per-file `cast_plan`, `buff_ability`, `make_unit_snapshot` etc. | Each test file independently |

Audit (after Phase 4b):
- ~12 test files have non-trivial inline helpers
- ~5 of those duplicate helpers that exist in `tests/common/`
- 3 "parity" files (`parity.rs`, `sim_parity.rs`, `legality_parity.rs`) share scene-setup patterns
- No documentation about which layout to pick when adding a helper

---

## 2. Target state

```
src/combat/ai/test_helpers/        ← split single file into module
  mod.rs                            (re-exports + thin facade)
  critic_harness.rs                 (CriticScenarioBuilder, run_critic, asserts)
  stage_harness.rs                  (StageTestHarness, stage-level asserts)
  unit_builder.rs                   (UnitBuilder, PlanBuilder)
  scenario.rs                       (CriticScenario type, helper constructors)

tests/common/
  mod.rs                            (re-exports + sub-module declarations)
  fixtures.rs                       (base_stats, test_equipment, default_*)
  apps/
    engine.rs                       (engine-only App: movement_app, init_engine_state)
    bridge.rs                       (bridge App: existing tests/common/bridge.rs)
    full.rs                         (full Bevy: future, currently inline in tests/combat/)
  scenarios/
    parity.rs                       (shared parity-test scene builders)
    statuses.rs                     (insert_haste_status, insert_armor_buff etc.)
```

Plus:
- `docs/testing.md` (✅ created — this commit cycle)
- CI: `cargo mutants --in-diff` on changed files in PRs

---

## 3. Migration phases

Each phase is a separate commit; tests stay green between phases.

### Phase H1 — Split `src/combat/ai/test_helpers.rs`

**Size:** 1 commit, ~1 hour.

Current file is 1 module with 4 conceptual sub-areas. Split into directory
with sub-modules. No behaviour change; pure organisation.

**Acceptance:**
- `cargo nextest run --workspace` green.
- `src/combat/ai/test_helpers.rs` becomes `mod.rs` of new directory.
- Public re-exports preserve all existing import paths
  (`use crate::combat::ai::test_helpers::CriticScenarioBuilder;` still works).

### Phase H2 — Audit and lift duplicates from inline helpers

**Size:** 1-2 commits, ~2-3 hours.

For each test file with non-trivial inline helpers, decide:
1. Used by 2+ files? → lift to `tests/common/`.
2. Used by 1 file? → leave inline; add comment explaining why it's local.

**Concrete candidates** (verified by grep during audit):
- `cast_plan(ability, target_entity, pos)` exists inline in
  `buff_into_void.rs`, `heal_without_rescue_value.rs` (and 4 more critic
  files) — lift to `test_helpers/scenario.rs`.
- `buff_ability(id, status_id)`, `heal_ability(id)`, `damage_ability(id)` —
  same pattern, lift to `test_helpers/builders.rs`.
- `hex_from_offset` — used in 20+ tests, currently re-imported each time.
  Add to `tests/common/mod.rs` re-exports.

**Acceptance:**
- Total LOC across test files drops by ≥ 300 (mostly removed duplicates).
- No new helpers in `tests/common/` are unused.
- `cargo nextest run --workspace` green.

### Phase H3 — Restructure `tests/common/` into submodules

**Size:** 1 commit, ~1 hour.

Move from flat `mod.rs` + `bridge.rs` to the directory layout in §2.
Re-export via `mod.rs` to preserve `common::base_stats` etc.

**Acceptance:**
- All existing test imports still compile without changes
  (re-exports cover the old paths).
- No file in `tests/common/` exceeds 400 LOC.
- `cargo nextest run --workspace` green.

### Phase H4 — Parity test consolidation

**Size:** 1 commit, ~1-2 hours.

Three parity files have overlapping scene-setup. Extract shared scenarios:

```rust
// tests/common/scenarios/parity.rs
pub struct ParityScene { /* engine + sim + ecs ... */ }
impl ParityScene {
    pub fn aoo_two_flankers() -> Self { ... }
    pub fn haste_speed() -> Self { ... }
    pub fn rage_aoe() -> Self { ... }
    // ...
}
pub fn run_engine_vs_sim(scene: &ParityScene) -> ParityResult { ... }
```

Rewrite the 3 parity files to consume `ParityScene`. Expect ~500 LOC saved.

**Acceptance:**
- All 12+ parity tests pass.
- Each parity file drops below 300 LOC.

### Phase H5 — Split mega-files (`#[path]` includes)

**Size:** 2 commits, ~30 min.

Apply the "split tests into sibling file when source > 1000 LOC and tests
> 40%" rule from `testing.md`. Concrete targets:

1. `src/combat/ai/world/snapshot.rs` (1704 LOC) →
   `snapshot.rs` (700 LOC production) + `snapshot_tests.rs` (1000 LOC tests).
2. `src/combat/ai/world/tags/classify.rs` (711 LOC) →
   `classify.rs` (200 LOC production) + `classify_tests.rs` (500 LOC tests).

Pattern:
```rust
// snapshot.rs (production)
#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
```

**Acceptance:**
- Both files compile.
- `cargo nextest run --workspace` green.
- IDE navigation works (jump-to-test works as before).

### Phase H6 — Pre-push mutation gate (CI)

**Size:** 1 commit, ~30 min — but requires CI infra (out of this plan's scope).

Add a `.github/workflows/mutation.yml` (or pre-push hook):

```yaml
- run: cargo install cargo-mutants
- run: cargo mutants --in-diff origin/main..HEAD --baseline=skip
```

Fail if mutation score on changed files regresses by > 5pp.

**Acceptance:**
- PR with intentional gap shows red CI.
- Documentation in `testing.md` §6 updated to mention CI gate.

---

## 4. Sequencing & risk

- **Phase H1 + H3 are zero-risk** — pure file moves, re-exports preserve imports.
- **Phase H2 + H4 are medium risk** — moving helpers can change usage patterns.
  Run mutants on changed files after each commit.
- **Phase H5 is zero-risk** — `#[path]` is well-supported by rustc and IDEs.
- **Phase H6 is gated by CI maintainer availability.**

Total estimated effort: **~6-8 hours** of focused work, split into 7 commits.

---

## 5. Non-goals

- **Not switching to `rstest` or similar dep.** Rejected in Phase 1.5 planning;
  manual table-driven loops keep maintenance lighter and naming uniform.
- **Not unifying critic harness with bridge harness.** They live at different
  layers (one inside `src/`, one inside `tests/`); merging them would force a
  shared layer that doesn't exist in production.
- **Not refactoring production code** (e.g. the `UnitView` / `UnitSnapshot`
  duplication noticed during Phase 4b). That's a separate plan.

---

## 6. Done-when

- All 5 implementation phases (H1-H5) merged.
- `docs/testing.md` references this plan as "completed".
- LOC across `tests/` and inline tests has not grown (target: down ≥ 800 LOC).
- New tests written after the refactor follow the decision tree in
  `testing.md` §8 without further coaching.
