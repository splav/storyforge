# Test Helpers Normalisation Plan

Plan to unify scattered test helpers into the layout documented in
[testing.md §3](../testing.md). One-off refactor; **not** an ongoing effort.

**Status:**

| Phase | Status | Commit |
|---|---|---|
| H5 — split mega-files via `#[path]` | ✅ Done | `b7fde9c` |
| H3 — restructure `tests/common/` | ✅ Done | `5630aa0` |
| H5b — split 6 more test-heavy AI files | ✅ Done | `7175918` |
| H0 — concrete audit | ✅ Done | (этот документ) |
| H1 — split `src/combat/ai/test_helpers.rs` | ❌ Attempted, reverted | — |
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

Audit signals **superseded by H0 audit** (see §3.H0). The original predictions
(`cast_plan`, `buff_ability` × 5+ duplicates each) **no longer match the codebase**
— those helpers were renamed or lifted between the original plan and the audit.
Current real duplicates are listed in §3.H0.

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

### Phase H0 — concrete audit (DONE)

**Method:** scanned all 2645 `fn` definitions in `src/` + `tests/`, grouped by
name, filtered trait impls (`new`, `default`, `deserialize`, `serialize`,
`deref`, `fmt`, `from`, `iter`, `apply`, `name`, `compute`, etc.), inspected
signatures of remaining candidates.

**Surprise:** original candidates from the pre-H0 plan **don't exist** in the
current codebase:

| Original candidate | Definitions | Status |
|---|---|---|
| `cast_plan` | 0 | Renamed/lifted before audit |
| `buff_ability` | 0 | Renamed/lifted before audit |
| `hex_from_offset` | 1 canonical in `src/game/hex.rs` | 88 usages, all importing canonical → not duplicated |

**Actual duplicates found:**

#### HIGH priority — clean lift, identical signatures (~70 LOC win, 1-2h)

| Helper | Signature | Files | Canonical | LOC saved |
|---|---|---|---|---|
| **`ent`** | `fn ent(id: u32) -> Entity` | **11** | `src/combat/ai/test_helpers.rs:30832` (already exists!) | ~30 |
| **`empty_plan`** | `fn empty_plan() -> TurnPlan` | **5** | Doesn't exist — create in `test_helpers` | ~25 |
| **`zero_needs`** | `fn zero_needs() -> NeedSignals` | **3** | Trivial — inline `NeedSignals::default()` | ~9 |
| **`unit` (3-arg)** | `fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot` | 2 (1 canonical + 1 dup in `aggregate_tests.rs`) | `test_helpers::unit` (already exists!) | ~5 |

**`ent` is the standout:** canonical helper has lived in `test_helpers.rs` for a
while, but 10 files independently reimplemented it inline. Authors likely
didn't discover the existing one. Pre-H1 H2-style lift of `ent` alone is
~10 trivial Edits.

#### MEDIUM priority — divergent signatures, needs Builder (~160-200 LOC, 3-5h)

| Helper | Files | Signature variants | Lift strategy |
|---|---|---|---|
| **`make_unit`** (Engine `Unit`, not `UnitSnapshot`) | 10 in `tests/combat_engine/*` + 1 in `saturation.rs` | 9 distinct: e.g. `(id, hp, max_hp)`, `(id, team, pos_col, pos_row)`, `(id, alive)`, `(id, team, reactions)`, ... | New `Unit::test_default(id).with_hp(N).with_team(T).build()` builder in `tests/common/apps/engine.rs`. Only `cast.rs` and `step.rs` have identical sigs — could be lifted as-is. |
| **`unit` (extended)** | 3 (`generator_tests`, `sim_tests`, `sanity/mod`) | `(id, team, pos, hp, max_ap)`, `(id, team, pos, hp, armor)`, `(id, team, pos, hp)` | Extend `test_helpers::UnitSnapshot::test_builder(id).with_pos(...).with_hp(...)` |
| **`move_plan`** | 6 in pipeline/stages/critics tests | 3 sig families: `()`, `(dest: Hex)`, `(path: Vec<Hex>)` | 3 helpers in `test_helpers`: `empty_move_plan`, `move_plan_to(dest)`, `move_plan_path(path)` |

#### Not duplicates (false positives)

- `team_of`, `unit_at_cell` — trait + 2 mock impls (`tests/combat_engine/targeting.rs`, `src/combat/ai/plan/parity_tests.rs`). Polymorphism, not duplication.
- `compute` × 22 — `StepFactor::compute` per-factor production code.
- `name`, `apply`, `new`, `default`, `from`, `iter` — standard trait impls.

**Realistic total LOC win:** ~230-270 (HIGH + MEDIUM combined). Plan's
"≥ 300 LOC" target reachable only with full MEDIUM phase (Builder pattern
introduction, ~5-7h of work). HIGH-only delivery is ~70 LOC for ~1.5h.

### Phase H2 — lift inline duplicates (PENDING, split into two sub-phases)

Concrete candidate list locked by H0. Split into two sub-phases for
incremental delivery:

**H2a — HIGH lifts only (1-2h, ~70 LOC win):**
1. `ent`: 10 inline copies → import existing `test_helpers::ent`. Mostly
   `Edit` work on 10 files.
2. `unit` (3-arg) in `aggregate_tests.rs`: 1 inline copy → import existing
   `test_helpers::unit`.
3. `empty_plan`: create `test_helpers::plan_helpers::empty_plan() -> TurnPlan`,
   replace 5 inline copies.
4. `zero_needs`: inline 3 callers to `NeedSignals::default()` directly,
   delete locals.

After each lift: `cargo nextest run --features dev` green.

**H2b — MEDIUM lifts (3-5h, ~160-200 LOC win):**
5. Introduce `Unit::test_default(id)` builder (engine Unit, not UnitSnapshot)
   in `tests/common/apps/engine.rs`. Migrate 10 `make_unit(...)` callers
   in `tests/combat_engine/*` to chain `.with_hp(...).with_team(...)` etc.
6. Extend `UnitBuilder` (or add `UnitSnapshotBuilder`) in `test_helpers` to
   cover `(hp, max_ap)`, `(hp, armor)`, `(hp)` overloads — migrate 3 callers.
7. Add `move_plan_to(dest)` / `move_plan_path(path)` / `empty_move_plan()` to
   `test_helpers::plan_helpers` — migrate 6 callers.

Acceptance:
- H2a alone: ≥ 60 LOC removed, 1151+ tests green.
- H2a + H2b: ≥ 230 LOC removed, no new helpers in `test_helpers` are unused.
- Per-helper green check (not batch — bisect-friendly).
- Run `cargo mutants` on `src/combat/ai/pipeline/stages/critics/`
  before+after H2 to confirm no coverage regressions from the moves.

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
