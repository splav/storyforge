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
| **Pure engine** | `crates/combat_engine/tests/*.rs` | `combat_engine::*` (no Bevy, no ECS). Replay, serde, RNG counting. | Lowest. Manual `CombatState` construction. |
| **Engine + bridge** | `tests/combat_engine/*.rs` | `engine_bridge.rs` — ECS projection, message translation, content view. | Medium. Bevy `App` with minimal plugins. |
| **Full app** | `tests/combat/*.rs` | End-to-end combat scenarios, AI decisions, animations. | Highest. Full `MinimalPlugins` + content load. |
| **Inline unit** | `#[cfg(test)] mod tests` inside source files | Single-function or struct-level contracts. | Module-local. |

**Rule:** test at the lowest layer that can prove the property. A pure
deterministic function → inline unit test. A schedule interaction → bridge
integration. A full turn cycle → app integration.

---

## 2. Where tests live

### Inline (`#[cfg(test)] mod tests`)

For pure functions, single-struct invariants, classifier rules. Lives in the
same `.rs` file as the code under test.

**Split into a sibling file when:**
- The source file exceeds **~1000 LOC** AND tests are >40% of it
- New convention: `<name>.rs` (production) + `<name>_tests.rs` (tests),
  attached via `#[cfg(test)] #[path = "<name>_tests.rs"] mod tests;`

Current files due for split (or already past the threshold):
- `src/combat/ai/world/snapshot.rs` — 1704 LOC, ~60% tests
- `src/combat/ai/world/tags/classify.rs` — 711 LOC, ~70% tests

### Integration (`tests/`)

`tests/combat/` — full app scenarios.
`tests/combat_engine/` — engine + bridge layer.
`tests/common/` — shared scaffolding (see §3).

One file per concern. Don't dump unrelated tests in the same file just because
they use the same harness (lesson from `bridge_smoke.rs` — 1834 LOC of mixed
movement/cast/projector/phase/trace).

### Pure-engine crate (`crates/combat_engine/tests/`)

For tests that should not depend on Bevy or game-side content. Currently:
`replay.rs`, `serde_roundtrip.rs`, `rng_count.rs`.

---

## 3. Where helpers live

Three valid locations, with a clear rule for each.

### `src/<module>/test_helpers.rs` — engine-private helpers

When tests need access to **private types or `pub(crate)` API** of the module
under test. Compiled with the crate.

**Examples:**
- `src/combat/ai/test_helpers.rs` — `CriticScenarioBuilder`, `UnitBuilder`,
  `StageTestHarness`, `assert_critic_fires/_passes`, `run_critic`.

**Rule:** use `#[cfg(any(test, feature = "test-helpers"))]` if the helper
should not bloat release builds. Most current helpers are inside
`#[cfg(test)]` modules — that's fine.

### `tests/common/` — integration test scaffolding

Shared across multiple integration test files in `tests/`. Pulled in via:
```rust
#[path = "../tests/common/mod.rs"] mod common;
```

**Current layout:**
- `tests/common/mod.rs` — cross-layer fixtures: `base_stats`, `test_equipment`,
  `movement_app`, `init_engine_state`, `insert_*_status` helpers.
- `tests/common/bridge.rs` — bridge-layer specific: `bridge_app`,
  `projector_only_app`, `spawn_caster/target/_with_speed`,
  `MeleeContent`, `write_move/cast`, `script_no_crit_fail`.

**Rule for new helpers:**
1. Helper is **needed by 2+ test files** → put in `tests/common/`.
2. Helper is **specific to one Bevy App configuration** (e.g. bridge_app vs
   movement_app) → put in `tests/common/<layer>.rs`.
3. Helper is **truly file-local** (only used by tests in one file) → keep
   inline; do **not** preemptively extract.

### File-local helpers (inline)

For test fixtures that are only used in one file. Live inside the
`#[cfg(test)] mod tests` of that file.

**Anti-pattern:** copying a helper from another test file. If you're tempted,
that's a signal to lift it to `tests/common/` instead.

---

## 4. Test categorisation

Group tests with section dividers (`// ── Section name ──`) by purpose:

| Category | Purpose | Failure means |
|---|---|---|
| **Regression pin** | Lock current behaviour on a specific content/scenario | Content changed OR rule changed |
| **Rule-based** | Test one rule branch with synthesised inputs | Production rule logic changed |
| **Property** | Invariant across many inputs (determinism, monotonicity) | Implementation broke a guarantee |
| **Parity** | Two implementations agree (engine vs sim, ECS vs engine) | One implementation drifted |
| **Smoke** | Full pipeline doesn't crash on a baseline scenario | Major regression |

**Why this matters:** when a pin test fails, the right response is usually
"update the snapshot/value". When a rule-based test fails, the right response
is "did I intend to change the rule?" Mixing them in the same file without
labels makes the right response ambiguous.

**Examples:**
- `classify.rs` § _Synthesized-input helpers_ + _Rule tests_ + _Special/regression tests_ — clean separation.
- `tests/combat_engine/parity.rs` — pure parity, file name signals intent.

---

## 5. Naming convention

`<subject>_<scenario>_<expected_outcome>` for rule-based tests:

```rust
fn ability_heal_to_ally_yields_rescue() { ... }
fn ability_heal_to_self_yields_no_rescue() { ... }
fn status_negative_speed_yields_soft_cc() { ... }
```

For pin/smoke tests: `<scenario>_<asserted_observation>`:

```rust
fn combat_2_bootstraps_fresh_after_combat_1() { ... }
fn engine_emits_combat_log_opportunity_attack() { ... }
```

Avoid: bare `test_X`, `it_works`, `case_1`. Tests are documentation — names
should read like specs.

---

## 6. Mutation testing

`cargo-mutants` (v27+) is the project's primary quality gate beyond
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

`--in-place` modifies the source on each iteration; **don't run two
mutants jobs in parallel on the same workspace** — they will race
(observed in this session).

**Targets:**
- Mutation score ≥ 85% for critical files (critics, classifier, dice, AoO).
- ≥ 70% for general AI logic.
- Save outcomes to `measurements/mutants-<name>-outcomes.json` for tracking.

**Known equivalent mutants** (cannot kill without changing production code):
document them in commit messages so reviewers don't re-fight the same battle.

### CI integration (planned, not yet enabled)

`cargo mutants --in-diff` on changed files in PRs. Fail when score regresses
on a touched file. See [refactor/helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) Phase 6.

---

## 7. Coverage

Secondary metric — coverage tells you what code is *executed*, not what's
*verified*. Use as a directional signal, not a gate.

```bash
cargo llvm-cov --html
open target/llvm-cov/html/index.html
```

Aim for **line coverage ≥ 75%** on `combat/` modules. Files dropping under
60% are flagged for review.

---

## 8. Adding a new test — decision tree

```
What are you testing?
│
├── A pure function or struct invariant?
│   → Inline unit test in source file.
│
├── A schedule / system interaction with ECS?
│   → tests/combat_engine/<concern>.rs (use tests/common/bridge.rs harness).
│
├── A full turn-cycle scenario?
│   → tests/combat/<concern>.rs (use tests/common/mod.rs).
│
├── A parity between two implementations?
│   → tests/combat/sim_parity.rs OR tests/combat_engine/parity.rs.
│
└── A regression for a specific bug?
    → Inline unit test or integration test at the layer where the bug
      manifested. Mark with `// regression: <ticket / commit>`.

Need a helper?
│
├── For one file → inline, in the test module.
├── For two+ files at the same layer → tests/common/<layer>.rs.
└── For two+ files needing private API of a module → src/<module>/test_helpers.rs.

Want to verify the test actually catches bugs?
└── Run mutants on the file under test (§6).
```

---

## 9. References

- [CLAUDE.md §3 Tests](../CLAUDE.md) — project test guidelines.
- [refactor/helpers-normalization-plan.md](refactor/helpers-normalization-plan.md) — planned consolidation.
- [combat/engine.md](combat/engine.md), [combat/bridge.md](combat/bridge.md) — what the layers test.
