# Test Helpers Normalisation Plan

Plan to unify scattered test helpers into the layout documented in
[testing.md §3](../testing.md). One-off refactor; **not** an ongoing effort.

**Status:**

| Phase | Status | Commit |
|---|---|---|
| H5 — split mega-files via `#[path]` | ✅ Done | `b7fde9c` |
| H3 — restructure `tests/common/` | ✅ Done | `5630aa0` |
| H1 — split `src/combat/ai/test_helpers.rs` | ❌ Attempted, reverted | — |
| H0 — concrete audit (grep `cast_plan`/`buff_ability`/`hex_from_offset`) | ⏳ Pending | — |
| H2 — lift inline duplicates | ⏳ Pending | — |
| H4 — shared parity scaffolding | ⏳ Pending (narrowed scope) | — |
| H6 — CI mutation advisory job | ⏳ Pending (infra-dep) | — |

After Plan-agent critique post-H5/H3, the plan was revised: re-ordered,
re-scoped, and hours re-estimated. See §3 below.

---

## 1. Current state

| Location | Purpose | Consumers |
|---|---|---|
| `src/combat/ai/test_helpers.rs` (~1065 LOC) | Critic harness, stage harness, unit/snapshot builders, asserts, caches, contexts | Inline tests in `src/combat/ai/pipeline/stages/critics/*.rs`, `src/combat/ai/world/snapshot_tests.rs`, several integration tests in `tests/` |
| `tests/common/` (post-H3) | Integration test scaffolding — restructured into `fixtures.rs`, `apps/engine.rs`, `apps/bridge.rs`, `scenarios/statuses.rs` | `tests/combat/*.rs`, `tests/combat_engine/*.rs` |
| **Inline locals** | Per-file `cast_plan`, `buff_ability`, `make_unit_snapshot`, etc. | Each test file independently |

Audit signals from the session that produced this plan:
- **6+ critic test files re-implement `cast_plan(ability, entity, pos)`** inline.
- **5+ files have a local `buff_ability(id, status_id)` helper.**
- **20+ test files re-import `hex_from_offset`** via the same path; could be re-exported from `tests/common/`.
- **3 "parity" files** (`crates/combat_engine/tests/parity.rs`, `tests/combat_engine/legality_parity.rs`, `tests/combat/sim_parity.rs`) — they test **different pairs**, so no body consolidation, but scene-setup builders may be sharable.

---

## 2. Target state

```text
src/combat/ai/test_helpers/        ← split monolithic file (H1)
  mod.rs                            (re-exports + thin facade)
  caches.rs                         (empty_*_tag_cache, EMPTY_*_TAG_CACHE)
  contexts.rs                       (make_test_ctx, make_scoring_ctx)
  unit_builder.rs                   (UnitBuilder + impl through build_pair)
  snapshot.rs                       (unit_snapshot_to_pair, snapshot_from*,
                                     unit_snapshot_to_engine_unit,
                                     unit_view_to_snapshot, unit, ent,
                                     empty_maps, empty_content)
  stage_harness.rs                  (StageTestHarness, PoolBuilder)
  critic_harness.rs                 (CriticScenario, CriticScenarioBuilder,
                                     run_critic, assert_critic_*,
                                     assert_stage_critic_*)

tests/common/                       ← Done (H3)
  mod.rs            (declarations + flat-path re-exports)
  fixtures.rs       (base_stats, test_equipment, hero/enemy bundles)
  apps/
    engine.rs       (movement_app, init_engine_state)
    bridge.rs       (bridge_app, projector_only_app, spawn_*, MeleeContent, ...)
  scenarios/
    statuses.rs     (insert_stun_status etc.)
    parity.rs       (ParityScene scene builders — H4, narrowed scope)
```

Plus:
- `docs/testing.md` ✅ in place.
- CI: advisory `cargo mutants --in-diff` job — H6.

---

## 3. Revised phases (post-critique)

**Revised order:** `H5 → H3 → H0 → H2 → H1 → H4 → H6`. H5 and H3 are
the lowest-risk and don't depend on the rest; doing them first proved the
pattern. H0 (audit) precedes H2 (lift duplicates) so the lifts are
fact-driven, not guesses. H1 (test_helpers split) comes after H2 so the
form is fully clear before partitioning.

**Revised hour estimates** (based on Phase 2's actual vs. planned drift):

| Phase | Planned | Realistic | Risk |
|---|---|---|---|
| H5 | 30min | **1-2h** | zero (done in 1h) |
| H3 | 1h | **2-3h** | low (done in ~1h actually) |
| H0 — concrete audit | — | **1h** (new step) | zero |
| H2 | 2-3h | **5-8h** | medium (touches 20+ files) |
| H1 | 1h | **2-4h** | medium (visibility/`pub use *` traps; H1 attempt #1 reverted after 1h of churn) |
| H4 — narrowed | 1-2h | **2-4h** (was 4-6h before scope cut) | medium |
| H6 | 30min | **2-4h** (CI infra) | infra-dep |

**Total realistic: 15-25 h** (was advertised as 6-8 h). Done so far: H5+H3 = ~3h.

### Phase H5 — split mega-files (DONE)

`snapshot.rs` 1703→826 LOC, `classify.rs` 711→164 LOC. Tests live in
sibling `<name>_tests.rs` files included via `#[cfg(test)] #[path = "..."]
mod tests;`. 1241 tests passing.

**Gotcha discovered:** arm64 macOS stale incremental cache produces
`_anon.<hash>` linker errors after the include change. `cargo clean -p
<crate>` resolves; documented in `testing.md §2`.

### Phase H3 — restructure `tests/common/` (DONE)

`tests/common/mod.rs` + `bridge.rs` → 5-file layout under `apps/` and
`scenarios/`. Backward-compat re-exports in `mod.rs` preserve all pre-H3
import paths. Zero changes to test files outside `tests/common/`.

### Phase H0 — concrete audit (PENDING, 1h)

Before H2, run real greps and turn them into a table:

```bash
rg -c "fn cast_plan\(" src/ tests/                  # count per-file
rg -c "fn buff_ability\(" src/ tests/
rg -c "use storyforge::game::hex::hex_from_offset" src/ tests/
```

Acceptance:
- A table in this plan: helper → files using it → suggested target location.
- Confidence about whether `cast_plan` actually has the same signature
  everywhere (it might not).

### Phase H2 — lift inline duplicates (PENDING, 5-8h)

After H0:
1. For each helper duplicated in 2+ places, lift to the target submodule.
2. Update inline imports across files; verify `cargo nextest run --workspace`
   green after **each** lifted helper (not at the end).
3. Run `cargo mutants` on `src/combat/ai/pipeline/stages/critics/`
   before+after to confirm no coverage regressions from the moves.

Acceptance:
- ≥ 300 LOC removed across test files (mostly duplicates).
- No new helpers in `tests/common/` are unused.
- 1241+ tests still green.

### Phase H1 — split `test_helpers.rs` (PENDING, 2-4h)

**Why deferred to after H2:** the "correct" sub-module shape only becomes
clear once we know which helpers H2 lifts. H1 attempt #1 produced ~16
visibility errors when `pub(crate) use sub::*` failed to re-export
`pub(crate)` items as expected. Reverted.

**Approach for retry:**
- Either bump all `pub(crate) fn` to `pub fn` in submodules (the module is
  test-only, so widened visibility is harmless).
- Or use explicit `pub use sub::{Item1, Item2}` instead of glob.
- Add cross-submodule imports explicitly (e.g. `use super::stage_harness::StageTestHarness`
  inside `critic_harness.rs`) — `use super::*` won't work for sibling
  references without re-exports from `mod.rs`.

Delegate to `thoughtful-implementer` with the visibility pattern pre-decided.

### Phase H4 — shared `ParityScene` builder (PENDING, 2-4h, NARROWED)

**Scope cut after Plan-agent critique:** the original plan claimed three
parity files share setup. Reality: they test **different pairs** (engine vs
sim, ECS bridge vs engine, real combat vs sim). No `run_*_vs_*` function
should be shared.

What **can** be shared: scene-setup builders that describe a battlefield
(unit positions, equipment, statuses). Extract into
`tests/common/scenarios/parity.rs`:

```rust
pub struct ParityScene {
    pub units: Vec<ParityUnit>,
    pub abilities: Vec<AbilityDef>,
    pub statuses: Vec<StatusDef>,
}
impl ParityScene {
    pub fn aoo_two_flankers() -> Self { ... }
    pub fn haste_speed() -> Self { ... }
    pub fn rage_aoe() -> Self { ... }
}
```

Each parity file applies the scene to its specific pair. Expect ~300 LOC
saved (not the ~500 originally estimated).

### Phase H6 — CI mutation advisory job (PENDING, 2-4h, REFORMULATED)

**Not a hard gate** — advisory only for the first 2-3 months. Reason:
`cargo mutants --in-diff` mutates entire new files (not just diff hunks),
which can blow up runtime on PRs that add files. Regression metrics across
diffs are noisy.

Add `.github/workflows/mutation-advisory.yml`:

```yaml
- run: cargo install cargo-mutants --version "^27"
- run: cargo mutants --in-diff origin/main..HEAD --baseline=skip
- if: failure() — post comment, don't fail PR
```

Acceptance:
- PR adding a new function shows "X mutants missed" advisory.
- Workflow doesn't block merge.

---

## 4. Sequencing & risk recap

- **H5 + H3 are done** — both were low-risk and unblocked the rest.
- **H0 is a 1h pure-audit step** with no code changes. Do it before H2.
- **H2 is the largest in hours** but mechanical once H0 produces a list.
- **H1 needs a focused retry** with a pre-decided visibility approach;
  delegate to `thoughtful-implementer`.
- **H4 is reduced** in scope after the critique.
- **H6 is advisory only**, separate track from H0-H4.

---

## 5. Non-goals

- **Not switching to `rstest` or similar dep.** Rejected in Phase 1.5 planning;
  manual table-driven loops keep maintenance lighter and naming uniform.
- **Not unifying critic harness with bridge harness.** They live at different
  layers; merging them would force a shared layer that doesn't exist in
  production.
- **Not refactoring production code** (e.g. `UnitView` / `UnitSnapshot`
  duplication noticed during Phase 4b). That's a separate plan — documented
  in `testing.md §10` as a known smell.
- **Not merging `parity.rs` / `sim_parity.rs` / `legality_parity.rs`** under
  a single shared engine — they test different pairs, see H4.

---

## 6. Done-when

- All 5 implementation phases (H0-H4) merged.
- `docs/testing.md` references this plan as "completed".
- LOC across `tests/` and inline tests has not grown (target: down ≥ 800 LOC
  cumulatively).
- New tests written after the refactor follow the decision tree in
  `testing.md §9` without further coaching.
- H6 advisory job runs on PRs without blocking merges, generating a measurable
  signal about coverage regressions.
