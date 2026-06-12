# Testing Guide

How tests are organised, where helpers live, and when to use which technique.
Living document — update when conventions change.

---

## 1. Test layers

Tests are layered to match the production architecture (see
[architecture.md](architecture.md), [combat/engine.md](combat/engine.md)).
Each layer tests its own contract; cross-layer concerns sit at the higher layer.

| Layer | Where | What it tests | Setup cost |
|---|---|---|---|
| **Pure engine** | `tests/combat_engine/*.rs` (e.g. `replay.rs`, `serde_roundtrip.rs`, `rng_count.rs`, `purity.rs`, `aura_determinism.rs`, `trace_helpers.rs`) | `storyforge::combat_engine::*` only — no Bevy, no ECS in the test body. Replay, serde, RNG counting, determinism. | Lowest. Manual `CombatState` construction. |
| **Engine + bridge** | `tests/combat_engine/*.rs` (e.g. `bridge_movement.rs`, `bridge_cast.rs`, `bridge_projector.rs`, `bridge_phase.rs`, `bridge_trace.rs`, `legality_parity.rs`) | модуль `src/combat/bridge/` — ECS projection, message translation, content view. | Medium. Bevy `App` with minimal plugins. |
| **Full app** | `tests/combat/*.rs` | End-to-end combat scenarios, AI decisions, animations. | Highest. Full `MinimalPlugins` + content load. |
| **Inline unit (engine-internal)** | `#[cfg(test)] mod tests` inside `crates/combat_engine/src/*.rs` | White-box tests of crate-**private** internals (e.g. `start_actor_turn`, content-hash, turn-queue). | Module-local. **Only built under `-p combat_engine` / `--workspace`** — see "Running" note below. |
| **Inline unit (storyforge)** | `#[cfg(test)] mod tests` inside `src/*.rs` (or sibling `<name>_tests.rs` via `#[path]`) | Single-function or struct-level contracts. | Module-local. |

> **Pure-engine tests live in the `storyforge` package now.** They build inside a
> Bevy-dependent package but stay Bevy-free in their *imports* (only
> `storyforge::combat_engine::*`). They are wired into the single
> `tests/combat_engine.rs` binary via `#[path]`. The old
> `crates/combat_engine/tests/` directory was removed — see "Running" below for why.
>
> **Running the full suite.** `cargo nextest run --features dev` (without
> `--workspace`) builds test targets of the **`storyforge` package only**, so the
> engine-internal inline tests in `crates/combat_engine/src/*.rs` are silently
> skipped. Use **`cargo nextest run --workspace --features dev`** to run everything
> (the `dev` feature applies to `storyforge` and is ignored for members that lack
> it). Do **not** add tests under `crates/combat_engine/tests/` — that target is
> only built under `-p combat_engine`, so it is silently excluded from the default run.

**Rule:** test at the lowest layer that can prove the property. A pure
deterministic function → inline unit test. A schedule interaction → bridge
integration. A full turn cycle → app integration.

---

## 2. Where tests live

### Inline (`#[cfg(test)] mod tests`)

For pure functions, single-struct invariants, classifier rules. Lives in the
same `.rs` file as the code under test.

**Split into a sibling file when** *(any* of):
- The source file exceeds **~1000 LOC**, OR
- Tests are >40% of the file, OR
- Production code is <30% of the file (clear sign tests dominate).

Convention: `<name>.rs` (production) + `<name>_tests.rs` (tests),
attached via `#[cfg(test)] #[path = "<name>_tests.rs"] mod tests;` at the
end of the production file.

**Completed splits:**
- `src/combat/ai/world/snapshot.rs` (was 1703 LOC) → `snapshot.rs` (826 LOC)
  + `snapshot_tests.rs` (895 LOC), three sub-modules: `affordability_tests`,
  `snapshot_api_tests`, `computation_tests`.
- `src/combat/ai/world/tags/classify.rs` (was 711 LOC) → `classify.rs`
  (164 LOC) + `classify_tests.rs` (555 LOC).

**Known gotcha (arm64 macOS, incremental build):** after a `#[path]` re-include,
`cargo build` may fail with `_anon.<hash>` linker errors from stale
incremental cache. Workaround: `cargo clean -p <crate>` once; subsequent
builds work normally.

### Integration (`tests/`)

`tests/combat/` — full app scenarios.
`tests/combat_engine/` — engine + bridge layer.
`tests/common/` — shared scaffolding (see §3).

**One file per concern** — but tolerate up to **~600 LOC** of mixed concerns
before splitting. The lesson from `bridge_smoke.rs` (was 2125 LOC of mixed
movement/cast/projector/phase/trace — now split into five focused files) is
that >1000 LOC of mixed concerns is unmaintainable; <600 LOC is usually fine
even if it spans 2-3 areas.

### Pure-engine tests (`tests/combat_engine/*.rs`)

Tests that exercise `combat_engine` with no Bevy / game-side content live
alongside the bridge tests in `tests/combat_engine/` (one binary, wired via
`tests/combat_engine.rs`). Keep their bodies Bevy-free — import only
`storyforge::combat_engine::*`. Examples: `replay.rs`, `serde_roundtrip.rs`,
`rng_count.rs`, `purity.rs`, `aura_determinism.rs`, `trace_helpers.rs`.

The `combat_engine` crate keeps **only** inline `#[cfg(test)]` modules (for
crate-private internals). There is intentionally no `crates/combat_engine/tests/`
directory — it would be silently excluded from the default run (see §1 "Running").

---

## 3. Where helpers live

Three valid locations, with a clear rule for each.

### `src/<module>/test_helpers.rs` — engine-private helpers

When tests need access to **private types or `pub(crate)` API** of the module
under test. Compiled with the crate.

**Examples:**
- `src/combat/ai/test_helpers.rs` (~1065 LOC) — `CriticScenarioBuilder`,
  `UnitBuilder`, `StageTestHarness`, `PoolBuilder`, `assert_critic_fires`,
  `assert_critic_passes`, `run_critic`, `assert_stage_critic_*`,
  `snapshot_from`, `unit_view_to_snapshot`, `empty_content`, etc.

**Rule:** keep `#![allow(dead_code)]` at the module top so unused items don't
spam warnings (the module is compiled in non-test builds because it's `pub mod`,
needed by integration tests). A split of this monolith into sub-modules is
planned in [helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) H1.

### `tests/common/` — integration test scaffolding

Shared across multiple integration test files in `tests/`. Pulled in via:
```rust
#[path = "../tests/common/mod.rs"] mod common;
```

**Current layout** (after H3 restructure):
```text
tests/common/
  mod.rs          — declarations + flat-path re-exports
  fixtures.rs     — base_stats, test_equipment, hero/enemy bundles,
                    enter_await_command, write_message, message_count
  apps/
    engine.rs     — movement_app, init_engine_state
    bridge.rs     — bridge_app, projector_only_app, spawn_caster/target/_with_speed,
                    spawn_enemy_with_weapon, MeleeContent, write_move/cast,
                    script_no_crit_fail/d20, with_engine_unit, no_equipment,
                    bridge_stats, default_equipment
  scenarios/
    statuses.rs   — insert_stun_status (and future insert_*_status helpers)
```

**Compatibility re-exports** in `mod.rs` preserve flat paths:
- `common::base_stats()` works (via `pub use fixtures::*`).
- `common::movement_app()` works (via `pub use apps::engine::*`).
- `common::bridge::bridge_app()` works (via `pub use apps::bridge`).

**Rule for new helpers:**
1. Helper is **needed by 2+ test files** → put in `tests/common/`.
2. Helper is **specific to one layer (engine / bridge / full app)** → put in
   `tests/common/apps/<layer>.rs`.
3. Helper is **truly file-local** (only used by tests in one file) → keep
   inline; do **not** preemptively extract.

### File-local helpers (inline)

For test fixtures that are only used in one file. Live inside the
`#[cfg(test)] mod tests` of that file (or in the `<name>_tests.rs` sibling).

**Anti-pattern:** copying a helper from another test file. If you're tempted,
that's a signal to lift it to `tests/common/` instead. Concrete known
duplicates: `cast_plan`, `buff_ability`, `hex_from_offset` (see
[helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) H2).

---

## 4. Test categorisation

Group tests with section dividers (`// ── Section name ──`) by purpose:

| Category | Purpose | Failure means |
|---|---|---|
| **Regression pin** | Lock current behaviour on a specific content/scenario, often as anchor for a known bug | Content changed OR rule changed |
| **Rule-based** | Test one rule branch with synthesised inputs | Production rule logic changed |
| **Property** | Invariant across many inputs (determinism, monotonicity) | Implementation broke a guarantee |
| **Parity** | Two implementations agree (engine vs sim, ECS vs engine) | One implementation drifted |
| **Smoke** | Full pipeline doesn't crash on a baseline scenario | Major regression |

**Why this matters:** when a pin test fails, the right response is usually
"update the snapshot/value". When a rule-based test fails, the right response
is "did I intend to change the rule?" Mixing them in the same file without
labels makes the right response ambiguous.

**Important nuance — regression pins are NOT consolidated.** If you have three
pin tests with structurally-similar bodies but each anchors a different
historical bug, **do not** merge them into one parameterised test. The
discrete failure (`pin_for_<bug>_no_longer_repros`) is the value. Mark them
with `// regression: <ticket / commit>` so reviewers know they're load-bearing.

**Examples:**
- `classify.rs` § _Synthesized-input helpers_ + _Rule tests_ + _Special/regression tests_ — clean separation.
- `tests/combat_engine/parity.rs` — pure parity, file name signals intent.

---

## 5. Naming convention

**Rule-based tests** (synthesised inputs, one branch per test) — tripartite:
`<subject>_<scenario>_<expected_outcome>`:

```rust
fn ability_heal_to_ally_yields_rescue() { ... }
fn ability_heal_to_self_yields_no_rescue() { ... }
fn status_negative_speed_yields_soft_cc() { ... }
```

**Pin / smoke / regression tests** — bipartite: `<scenario>_<asserted_observation>`:

```rust
fn combat_2_bootstraps_fresh_after_combat_1() { ... }
fn engine_emits_combat_log_opportunity_attack() { ... }
```

Avoid: bare `test_X`, `it_works`, `case_1`. Tests are documentation — names
should read like specs.

**Pre-existing tests should not be renamed** just to fit the convention —
`git blame` integrity matters more than uniform style. Apply the convention
only to new tests.

---

## 6. Mutation testing

`cargo-mutants` (v27+) is the project's primary quality signal beyond
coverage. It mutates one operator/return-value at a time and re-runs tests;
mutants the suite doesn't catch indicate gaps.

**When to run:**
- After adding tests for a new function — verify tests actually discriminate.
- When touching critical paths (combat resolution, AI scoring, engine step).
- Before declaring "this module is well-tested".

**How to run:**
```bash
cargo mutants --in-place --file 'src/path/to/file.rs' \
  --output measurements/mutants-<name>/ -- --lib
```

`--in-place` modifies the source on each iteration. Two warnings:

1. **Don't run two mutants jobs in parallel** on the same workspace — they
   race (observed in this session).
2. **Don't keep rust-analyzer indexing the workspace** while mutants runs —
   the IDE will re-index on every mutation cycle. Use `--copy-target` to a
   separate target directory, or close the IDE during mutation runs.

**Targets:**
- Mutation score ≥ **85%** for critical files (critics, classifier, dice,
  AoO, engine `step()`).
- ≥ **70%** for general AI logic.
- Track outcomes in `measurements/mutants-<name>-outcomes.json` per run.

**Timeouts ≠ uncaught mutants.** `cargo mutants` reports `timeout` separately
from `missed`. Timeouts mean the mutated code caused an infinite loop somewhere
in the test suite — typically a fuzz-protection gap rather than a coverage
gap. Don't include timeouts in the score numerator. Investigate the loop
separately (e.g., `snapshot.rs` had 18 timeouts in iterators like `enemies_of`
that downstream tests assumed always terminate).

**Equivalent mutants** (semantically-identical replacements like `* → /` when
both operands force the operation into a non-effective range) cannot be killed
without changing production code. Track them in
`measurements/equivalent-mutants.md` so future runs don't re-fight the same
battle. Citing them in commit messages alone is too easy to lose.

### CI integration (planned, not yet enabled)

`cargo mutants --in-diff` on changed files in PRs as an **advisory job**
(not a hard gate). Why advisory:
- `--in-diff` mutates entire new files, not just diff hunks. A 50-line new
  file generates ~30 mutants — CI runtime balloons.
- Score comparisons across diffs (regression > Npp) are noisy.

See [helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) H6
for the rollout plan.

---

## 7. Coverage

Secondary metric — coverage tells you what code is *executed*, not what's
*verified*. **Directional signal, not a CI gate.**

```bash
cargo llvm-cov --html
open target/llvm-cov/html/index.html
```

Workspace currently sits at ~79% line coverage; that did **not** prevent
critics from being at 44% mutation score before this session's reinforcement
work. Coverage drift triggers a *manual* review, not an automated block.

---

## 8. Determinism, slow tests, and flake-resistance

### RNG seeds

Combat and AI tests must be **deterministic**. Every test that touches dice
or randomness either:
- Uses `DiceRng::scripted(&[...])` with explicit value sequences (preferred),
  via `script_no_crit_fail(app)` / `script_d20(app, value)` in the bridge harness.
- Uses a fixed seed (`DiceRng::seeded(0xDEAD_BEEF)`).

**Never** use `DiceRng::new()` (random seed) in tests. CI re-runs would flake.

### Slow tests

Tests with high setup cost (full content load, scenario simulation > 500 ms)
should be flagged so they can be excluded from fast iterations:
- Add `#[ignore = "slow: <reason>"]` and run via `cargo nextest run
  --run-ignored only --profile slow`.
- Track the nextest `slow` profile in `.config/nextest.toml`.

### No `thread::sleep` in tests

If a test needs to wait for a Bevy schedule tick to complete, drive it with
`app.update()` — never `sleep`. Sleep-based tests cause CI flakes on busy runners.

---

## 9. Adding a new test — decision tree

```
What are you testing?
│
├── A pure function or struct invariant?
│   → Inline unit test in source file.
│
├── A schedule / system interaction with ECS?
│   → tests/combat_engine/<concern>.rs (use tests/common/apps/bridge.rs harness).
│
├── A full turn-cycle scenario?
│   → tests/combat/<concern>.rs (use tests/common/apps/engine.rs harness).
│
├── A parity between two implementations?
│   ├── Engine internals vs sim (no ECS) → tests/combat_engine/parity.rs
│   │   (or another Bevy-free tests/combat_engine/<concern>.rs).
│   ├── ECS-bridge legality vs engine legality → tests/combat_engine/legality_parity.rs.
│   ├── Sim-vs-shared-core parity (drift dimensions: speed reflow, AoO, rage) → src/combat/ai/plan/parity_tests.rs (Layer 1b section).
   └── True full-app real-vs-sim parity (drive BOTH real pipeline AND sim, then diff) → NOT YET IMPLEMENTED.
│
├── A read of asset/TOML content?
│   → Inline test in `content/` module if it's about parsing; otherwise an
│     integration test that calls `ContentView::load_global_for_tests()`.
│
└── A regression for a specific bug?
    → Inline unit test or integration test at the layer where the bug
      manifested. Mark with `// regression: <ticket / commit>`.

Need a helper?
│
├── For one file → inline, in the test module.
├── For two+ files at the same layer → tests/common/apps/<layer>.rs OR
│   tests/common/fixtures.rs (cross-layer).
├── For two+ files needing private API of a module → src/<module>/test_helpers.rs.
└── Status injection / scenario setup → tests/common/scenarios/<topic>.rs.

Want to verify the test actually catches bugs?
└── Run mutants on the file under test (§6).
```

---

## 10. Known design smells (informational)

These are **not** blockers for writing tests, but if you notice them while
working, they may surface in code review:

- **`UnitView` and `UnitSnapshot` have duplicated methods** (`eff_hp`,
  `eff_max_hp`, `hp_pct`, `killability`, `can_afford`, `is_alive`). Tests
  must cover both ports independently. Production-code unification is a
  separate refactor — not a test problem.
- ~~`tests/combat_engine/bridge_smoke.rs` (mixed-concern monolith)~~ **RESOLVED**
  (C4): split into `bridge_movement.rs` (8 tests), `bridge_projector.rs` (4),
  `bridge_cast.rs` (7 + 2 helpers), `bridge_trace.rs` (3), `bridge_phase.rs` (3).
  Add new bridge tests to whichever file matches their concern.
- **`src/combat/ai/test_helpers.rs` (~1065 LOC)** is a monolithic helper module
  with mixed concerns (caches, contexts, unit builder, snapshot helpers,
  stage harness, critic harness, assertions). Split planned but not yet
  applied — see H1 in the normalisation plan.
- **June 2026 full revision** found a further batch of issues — most notably
  tests that don't verify what their names claim (`parity_*_real_vs_sim`
  without a real-engine leg, parity-only legality cases) and systemic
  boilerplate duplication (template litanies, Bevy content-def wrappers,
  headless-app builders). Plan with per-item fixes:
  [refactor/test-revision-2026-06.md](refactor/test-revision-2026-06.md).

---

## 11. References

- [CLAUDE.md §3 Tests](../CLAUDE.md) — project test guidelines.
- [refactor/helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) — planned helper consolidation.
- [refactor/test-revision-2026-06.md](refactor/test-revision-2026-06.md) — June 2026 suite revision: honesty fixes + dedup plan (R1–R5).
- [combat/engine.md](combat/engine.md), [combat/bridge.md](combat/bridge.md) — what the layers test.
