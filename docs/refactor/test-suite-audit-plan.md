# Test-suite audit & rework plan

> Status snapshot: **Phase 0 committed** (`572d197`). **Phase 1 complete & green, uncommitted** (all 15 files migrated, suite 1332/1 skipped — see "Current state" at the bottom). Phases 2–4 not started.
>
> Recovered from a session that broke on an `API Error: 400 … thinking blocks cannot be modified`. This file is the canonical handoff.

## Context

The audit started from a concrete bug: crate-level integration tests under
`crates/combat_engine/tests/*.rs` were **silently excluded** from the canonical
test command `cargo nextest run --features dev` (because `dev` is a feature of
the root `storyforge` package, so cargo only builds storyforge's test targets,
never the `combat_engine` workspace member's). Those files had also rotted
against the current API (HP-as-pool refactor, `requires_los`, `EffectSource`).

That was resolved earlier (migrate-into-active-binary work, commits `02434ff` /
`987b5ed`). This plan covers the **follow-on quality audit**: a thorough
inspection of the *active* suite (`tests/combat_engine/*.rs` + inline
`#[cfg(test)]`) for redundant, obsolete, oversized, and zero-value tests, plus
structural dedup.

Canonical full run (must stay green): `cargo nextest run --workspace --features dev`
— baseline **1348 → 1332 passed / 1 skipped** after Phase 0.

## Locked decisions (from the user)

1. **sim_parity.rs** → delete the stub `ParityReport`/`run_parity_scenario`
   scaffold (Phase 0, done); relocate the honest `*_real_vs_sim` tests to the AI
   layer (Phase 3).
2. **role.rs:531** legacy snapshot → **retire** (Phase 4; legacy fn already deleted).
3. **Parameterization mechanism** → add **`rstest`** as a dev-dependency (Phase 4).
4. **los_parity.rs:204** ("all_three_backends" runs only 2) → **add the missing
   bevy backend**, not rename (done in Phase 0).

## The 5-phase plan (low-risk first)

| Phase | What | Effort | Risk | Status |
|---|---|---|---|---|
| **0** | Pure deletions & renames (zero-value / tautological tests) | S | Very low | ✅ committed `572d197` |
| **1** | Shared engine-`Unit` builder + `StubContent` (kill ~20× `make_unit` + stub explosion) | L | Low-med | ✅ complete & green, uncommitted |
| **2** | Reuse `bridge_app()` + dedup `replay_bin` | M | Low-med | ✅ complete & green, uncommitted |
| **3** | Relocate layer-inverted tests + finish sim_parity relocation | M | Medium | ⬜ not started |
| **4** | Parameterize/split too-long tests (rstest), retire `role.rs:531` snapshot | L | Medium | ⬜ not started |

**Ordering logic:** delete dead tests first (shrinks surface before refactoring),
then land the shared builder (later phases depend on it), then the
coverage-sensitive refactors. Graph confirmed the ~20 `make_unit` copies are
leaf consumers — nothing depends on them, so the builder migration is purely
additive, one file per commit.

**Key sequencing fact:** `tests/combat_engine.rs` already imports `common`, so a
builder dropped in `tests/common/` is instantly usable by all 22 engine modules.
The two top-level files that "can't import common" just need one
`#[path="common/mod.rs"] mod common;` line each.

**Suggested stopping point:** end of Phase 2 captures nearly all duplication wins
plus every safe deletion at low-to-medium risk. Phases 3–4 are
higher-judgment, coverage-sensitive work.

**The one trap to watch (Phase 4):** several heal/damage tests re-encode the
scoring formula in their expected values. When table-driving them, convert to
*behavioral/relative* asserts ("near-death heal ≥ full-HP heal"), never copy the
formula — otherwise parameterization entrenches the formula-echo.

---

## Phase 0 — done (committed `572d197`, 19 files, +54 / −421)

Pure deletions that cannot catch a regression by construction, plus a few
coverage-tightening rewrites.

**Clear deletions (8):** 3 `default()==default()` `considerations.rs` tests · 2
formula-echo `policy/tests.rs` tests · 2 in-test-body-assert `equipment.rs`
tests · `measure_trace_size_per_round` println-bench · the `sim_parity.rs`
`ParityReport`/`run_parity_scenario`/`parity_no_op` stub scaffold · duplicate
`repair/tests.rs` table · `state.rs pools_canonical_after_spawn` · `entity_id.rs
round_trip`.

**Nuanced items (coverage preserved, not deleted):**
- **offensive.rs** — kept the integration assert (`off_high.damage <
  off_low.damage`), removed only the redundant friendly-fire formula re-assert.
- **step.rs** — only test exercising 6 simultaneous AoOs; kept it, renamed
  `step_recursion_depth_capped` → `deep_reaction_chain_all_resolve`, replaced the
  misleading 30-line essay comment with an accurate description.
- **effect.rs** — only `Effect::Death` test; rewrote to start from `hp=10` so the
  effect actually drives hp→0, added `assert_eq!(hp(), 0)`.
- **future_value.rs:723** — ordering was deterministic; added the real assertion
  `assert!(scores[1] > scores[0])` instead of renaming.

**Collapse:** `cc.rs` 4 tests → 1 (`cc_value_zero_and_additive`).
**Trims:** dead setup removed from `turn_queue.rs`, `reaction.rs`.
**Renames:** `_sirota`→`_orphaned`; stale `baseline_v36/v38`→`v44` in
`golden_smoke.rs` + `ai_scenarios.rs`.
**Decision 4:** `los_parity.rs` property test now wires in the bevy backend
(`n_cases` 200→60 since each Bevy case spins a `MinimalPlugins` app).
CLAUDE.md test-count figure updated.

**Side note:** `benches/engine_move.rs:37 forced_mode` diagnostic is a
**pre-existing** benchmark breakage (benches aren't compiled by the test suite),
unrelated to this work. A task chip was dropped for it.

---

## Phase 1 — shared `EngineUnitBuilder` + `StubContent` (IN PROGRESS)

Goal: eliminate ~20 copy-pasted unit-construction helpers and ContentView stubs
across the engine test suite by introducing TWO shared fixtures in
`tests/common/engine_unit.rs`, then migrating call sites. **Pure mechanical
refactor — no test logic/assertions/count change** (stays 1332 / 1 skipped).

### Verified ground-truth APIs
- Constructor: `Unit::new(id: UnitId, team: Team, pos: Hex, speed: u32,
  initiative: i32, pools: EnumMap<PoolKind, Option<Pool>>, regens: EnumMap<PoolKind,
  i32>, weapon: Option<Weapon>, armor_class: u32) -> Unit` (crates/combat_engine/src/state.rs).
- `Pool::new(current, max)`; `UnitId(u32)`.
- `PoolKind`: `Hp, Mana, Ap, Mp` (+ `Energy, Rage` — confirm full set). Pools via
  `enum_map::enum_map!{ PoolKind::Hp => Some(Pool::new(hp,hp)), _ => None }`;
  regens via `enum_map!{ _ => 0 }`.
- `ContentView` trait (content.rs:50) — EXACTLY 4 methods: `ability(AbilityId) ->
  Option<&AbilityDef>`, `status(StatusId) -> Option<&StatusDef>`,
  `weapon(WeaponId) -> Option<&Weapon>`, `aoo_dice() -> Dice`.
- Canonical helper shape (determinism.rs): `make_unit(id, team, col, row, hp)` —
  takes col/row, NOT a pre-built Hex; replicate determinism.rs's exact
  col/row→Hex conversion verbatim in the builder's `.pos()`.
- Ergonomics reference: AI-side `UnitBuilder` in `src/combat/ai/test_helpers.rs`
  (chained setters, defaults hp=10/10, speed=6, team=Player, `.build()`).
- ⚠️ `MapContentView` does NOT exist (the recovered session's Read tool
  hallucinated it). Don't reference it.

### (a) `EngineUnitBuilder` — chained, consuming `self`
Defaults: team=Player, pos=(0,0), Hp=(10,10), Ap=(2,2), Mp=(6,6),
Mana/Energy/Rage=None, speed=6, initiative=0, weapon=None, armor_class=0, no
statuses, regens=0.
Setters: `new(id: u32)`, `team`, `pos(col,row)`, `hp(cur,max)`, `hp_full(hp)`,
`speed`, `initiative`, `ap`, `mp`, `mana`, `energy`, `rage`, `regen(PoolKind,i32)`,
`weapon`, `armor_class`, `status(ActiveStatus)`, `build() -> Unit`.

### (b) `StubContent` — configurable `ContentView`
Holds `HashMap<AbilityId,AbilityDef>`, `HashMap<StatusId,StatusDef>`,
`HashMap<WeaponId,Weapon>`, `aoo: Dice` (default `Dice::flat(0)` or
zero-equivalent). Config: `new()`/`Default`, `with_ability`, `with_status`,
`with_weapon`, `aoo_dice(Dice)`. Implements the 4 trait methods via map lookups.

### Migration steps
- **STEP 1** — create `tests/common/engine_unit.rs` (`#![allow(dead_code)]`), add
  `pub mod engine_unit;` to `tests/common/mod.rs`.
- **STEP 2** — prove the API on `determinism.rs` first (note: it defaults
  Mana=20/20 — verify). Iterate builder until ergonomic before touching others.
- **STEP 3** — identical-shape files: `aura_determinism.rs`, `rng_count.rs`,
  `turn_queue.rs`, `trace_helpers.rs`, `serde_roundtrip.rs`, `aura.rs`, `trap.rs`,
  `end_turn.rs`. Use a thin per-file `make_unit` wrapper where a file has
  nonstandard pool defaults.
- **STEP 4** — richer files (verify behavior): `reaction.rs` (weapon + nonzero
  aoo dice), `phase.rs` (`make_boss`/`make_attacker` + phase-specific content),
  `replay.rs` (≥4 `NoContent`/`DamageContent` stub copies).
- **STEP 5** — full-app / top-level files (add `#[path="common/mod.rs"] mod
  common;` if missing): `tests/combat/handoff.rs`, `tests/replay_diff_smoke.rs`,
  `tests/temporary_ally_e2e.rs`.

### Constraints
- No assertion / test-name / test-count change. Mechanical only.
- Reproduce every non-default pool (Mana=20/20, weapons, nonzero aoo dice). When
  in doubt keep a thin per-file `make_unit` delegating to the builder — still
  removes the `enum_map!`/`Pool` boilerplate (the real win).
- Remove now-unused imports from migrated files.
- If a file can't be migrated without changing behavior, STOP and report it.
- End: `cargo nextest run --workspace --features dev` → 1332 / 1 skipped. Then
  `graphify update .`.

---

## Phase 2 — reuse `bridge_app()` + dedup `replay_bin` (DONE, green, uncommitted)

- **Part A:** deleted the local `bridge_app()` reimplementation in
  `tests/combat_engine/legality_parity.rs` (verified byte-equivalent to the
  shared one), pointed its 2 call sites at `common::apps::bridge::bridge_app()`,
  pruned the now-unused resource/system imports.
- **Part B:** extracted `tests/common/bin.rs::sibling_bin(name)` (a
  profile-agnostic `current_exe` walk-up) and routed all three duplicated
  binary-path helpers through it: `golden_smoke.rs` + `combat/replay_assert.rs`
  (both `replay_ai_log`) and `replay_diff_smoke.rs` (`replay_diff`, previously a
  hardcoded `target/debug/` path — now profile-agnostic). `golden_smoke.rs`
  gained a `#[path="common/mod.rs"] mod common;` declaration.
- Net ~−77 LOC. `cargo nextest run --workspace --features dev` → 1332 / 1 skipped.

## Phase 3 — relocate layer-inverted tests (not started)

Move tests that live at the wrong layer to their proper home; relocate the
honest `sim_parity` `*_real_vs_sim` tests to the AI layer (scaffold already
deleted in Phase 0).

## Phase 4 — parameterize / split too-long tests (not started)

Add `rstest` dev-dep; table-drive the long heal/damage tests **with
behavioral/relative asserts, never formula echoes**; retire the `role.rs:531`
legacy snapshot; split oversized tests.

---

## Current state

- **Committed:** Phase 0 = `572d197`.
- **Phase 1 — DONE, green, uncommitted working tree:**
  - New: `tests/common/engine_unit.rs` + `pub mod engine_unit;` in
    `tests/common/mod.rs`. `EngineUnitBuilder` + `StubContent`. (The builder grew a
    `.template(impl Into<String>)` setter to migrate the `test_template` unit ctor
    in `temporary_ally_e2e.rs`.)
  - Migrated (all 15 call-site files): `determinism.rs`, `aura_determinism.rs`,
    `rng_count.rs`, `turn_queue.rs`, `trace_helpers.rs`, `serde_roundtrip.rs`,
    `end_turn.rs`, `trap.rs`, `reaction.rs`, `phase.rs`, `replay.rs` (first pass)
    plus `aura.rs`, `tests/combat/handoff.rs`, `tests/replay_diff_smoke.rs`,
    `tests/temporary_ally_e2e.rs` (this pass).
  - **Intentionally NOT migrated:** `aura.rs`'s `AuraContent` — it doubles as the
    aura-geometry config carrier (`with_aura` reads its `radius`/`status_id`/
    `applies_to`), so it is not a plain `ContentView` stub. Left untouched.
  - `cargo nextest run --workspace --features dev` → **1332 passed / 1 skipped**.
    `graphify update .` run.
- **Pre-existing unrelated breakage:** `benches/engine_move.rs:37` missing
  `forced_mode` — a benchmark file, not compiled by the test suite. Untouched by
  this work; tracked separately.

### Immediate next actions
1. **Commit Phase 1** (not yet committed).
2. Proceed to **Phase 2** — reuse `bridge_app()` + dedup `replay_bin`.
