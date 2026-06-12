# AI ↔ Engine ↔ ECS/Bridge — structure audit (2026-06-12)

Snapshot of an architecture review at commit `d8a83dd` (+ uncommitted magic_resist work).
Goal: cleaner responsibilities, simpler abstractions, less duplication across the
AI / engine / ECS-bridge layers.

**TL;DR:** The architecture has already converged to a sound shape — a pure engine
owning all mutations, a single bridge projecting into ECS, and an AI that simulates
by calling the *real* engine `step()` (unisim). The biggest remaining wins are not
redesigns but **finishing migrations that stopped halfway**: a 650-line dead parallel
ability-resolution core in `ai/sim/`, a vestigial `UnitSnapshot` mirror, a triple
representation of content definitions, a 2550-line bridge monolith, and docs that
describe the pre-unisim world.

---

## How it's actually structured today

- **Engine** (`crates/combat_engine`, pure Rust): `step(state, action, rng, content)`
  is the sole mutation point; events stream out; `ContentView` trait abstracts content;
  determinism pinned by trace schema v44 + purity tests.
- **Bridge** (`src/combat/engine_bridge.rs`, 2550 lines): the only file touching both
  worlds — bootstrap (`from_ecs`), `process_action_system` → `step()`, `translate_one`
  (events → log/queues), `project_state_to_ecs` (D6 contract, enforced by
  `tests/projection_isolation.rs`).
- **AI**: `BattleSnapshot` (since schema v38) is literally **engine `CombatState` +
  `AiCache`** of AI-only per-unit data (`src/combat/ai/world/snapshot.rs:24`); plan
  simulation (`src/combat/ai/plan/sim.rs`) clones that state and calls engine `step()`
  with an `ExpectedValue` dice source — *zero sim drift by construction*. Legality is
  one engine function (`check_legality`) behind a 3-backend `ActionState` trait where
  LOS has a single physical copy (trait default).

That's a genuinely clean core. The problems are residue around it.

---

## Findings, ranked

### 1. Dead parallel sim core — `src/combat/ai/sim/` (~650 LOC) — delete it

`effects_outcome.rs` (552 lines, `compute_ability_outcome`) claims to be "the shared
ability-resolution core for live pipeline and AI sim". That was true pre-unisim; today
it has **zero production callers** — only its own inline tests and
`src/combat/ai/plan/parity_tests.rs`. The files it claims to share with
(`src/combat/effects_*`, `combat/resolution.rs`) no longer exist.
`effects_state.rs::compute_affected_targets` duplicates
`crates/combat_engine/src/targeting.rs::compute_affected_targets` (the one `step()`
actually uses, `step.rs:663`).

**Improvement:** delete `ai/sim/` and its parity tests (the parity they guard is now
vacuous — both sides of the comparison should just be the engine). Keep one regression
test asserting the *scoring*-side AoE helper agrees with engine targeting.
**Effort:** ~half a day. **Risk:** low — `cargo check` will prove the absence of callers.

Related: `src/combat/ai/scoring/factors/aoe_hits.rs` is a third AoE-enumeration
implementation ("parallel to effects_state…" per its own header). It earns its keep
(returns team-split `UnitView`s for scoring), but after deleting `ai/sim/` its doc
comment should point at engine `targeting` as the canonical sibling, and ideally it
should delegate cell/target enumeration to `combat_engine::targeting::aoe_cells` so
only the team-splitting stays AI-side.

### 2. `UnitSnapshot` — a vestigial mirror of engine `Unit`

`UnitSnapshot` (`snapshot.rs:78`) duplicates ~15 engine `Unit` fields (hp, armor,
armor_bonus, damage_taken_bonus, base_speed/speed, statuses, reactions, summoner,
caster context…) alongside AI fields. But the modern data model already exists:
`BattleSnapshot.state` (engine truth) + `UnitAiCache` (AI-only: threat, tags, role,
aoo_expected_damage…), composed by `UnitView` (Deref to `&Unit`). `UnitSnapshot`
survives in ~15 files — scoring leaves (`policy/status.rs`, `step/scarcity.rs`,
`step/saturation.rs`, `horizon.rs`), `plan/reach.rs`,
`pipeline/stages/item_scoring.rs`, test helpers.

**Improvement:** migrate the remaining consumers to `UnitView` and delete
`UnitSnapshot` (and its serde plumbing + the `refresh_aggregates` contract described
in ai.md, which only exists to keep the mirror honest). This kills an entire class of
"which copy of hp do I read?" bugs.
**Effort:** 1–2 days (mechanical, but touches log schema — old-log deserialization
already ignores unknown fields, so likely no bump). **Risk:** medium-low; golden
replay corpus (`tests/baselines/baseline_v34.jsonl` + `golden_smoke`) is the guard.

### 3. Content layer: three parallel definitions + a name collision

For abilities/statuses the same concept exists as:

1. App parse structs — `src/content/abilities.rs:92`, `src/content/statuses.rs:21`
   (Bevy-tied via `CombatStats`/`Equipment`);
2. Engine canonical — `crates/combat_engine/src/content.rs` (`AbilityDef`,
   `StatusDef`, …), converted via `src/content/to_engine.rs`;
3. Engine offline TOML parser — `crates/combat_engine/src/toml_content_view.rs`
   (825 lines) which **re-duplicates the TOML record structs and mapping** for replay
   tooling, self-acknowledged in its header ("Phase 5 D9 Path A… This file
   duplicates…").

Every new content field (e.g. the in-flight `magic_resist`) must be threaded through
3 struct families + 2 converters. On top, **`ContentView` names two different
things**: an app-side struct (merged content layers, `src/content/content_view.rs:26`)
and the engine trait — `plan/sim.rs` imports both with an alias, and every reader pays
the disambiguation tax.

**Improvement (incremental, in order of value):**
- Move the engine-relevant TOML *wire structs* + parsing into the engine crate
  (they're already Bevy-free in `toml_content_view.rs`) and make `src/content` parse
  the Bevy-only extras *around* that shared parser — collapsing 3 representations to 2
  and deleting most of the 825-line duplicate. **Effort:** 2–3 days. **Risk:** medium
  (content hash / determinism contract — parity test already exists to pin it).
- Rename the app struct `ContentView` → `ActiveContentData` or similar (it's the
  payload of `ActiveContent` anyway). Mechanical. **Effort:** hours.

### 4. `engine_bridge.rs` is a 2550-line, ~8-concern monolith

The doc says ~1815 lines; it's 2550 and growing. Distinct concerns per the outline:
id mapping (`UnitIdMap`), bootstrap (`from_ecs` 156 lines, `build_unit`,
`build_engine_template_from_def`), `EcsContentView`, queues (`BridgeQueues` + 2 drain
systems), phase ECS writes, dynamic spawn (`spawn_ecs_entity_from_engine_unit`),
event translation (`translate_one` — a 400-line match), action processing
(`process_action_system`, 312 lines), projection (`project_state_to_ecs`, 185 lines),
reset systems. The "single file talks to both sides" rule is good; a single *module*
would serve it equally.

**Improvement:** split into
`src/combat/bridge/{ids, bootstrap, content_view, translate, process, project, queues, phases}.rs`
with a `mod.rs` re-export keeping the public surface identical. The
projection-isolation test keeps enforcing the write contract (update its path
allowlist). **Effort:** ~1 day, purely mechanical. **Risk:** low.

### 5. Docs drifted at exactly this layer

- `docs/ai/ai.md` "Shared effects core (вне ai/)" points to
  `src/combat/effects_math.rs`/`effects_state.rs`/`effects_outcome.rs` and
  `combat/resolution.rs` — **none exist**; the real story is "sim calls engine
  `step()`".
- ai.md's whole "Mid-plan reflow" section (manual rage mirroring of
  `apply_effects.rs:117-129`, hand-rolled AoO propagation, `refresh_aggregates`
  contract) describes the **pre-unisim** sim; `plan/sim.rs:1` explicitly says the
  opposite ("no separate hand-rolled damage math").
- bridge.md's line count and `translate_one` line ranges are stale.

Misleading docs here are worse than missing ones — they tell a reader the sim drifts
when it can't. **Effort:** ~half a day, do together with items 1–2 so it's written once.

### 6. Already-known debt (documented, still real)

- **Phase overrides split across engine and bridge** (`docs/combat/bridge.md §10`):
  engine's `EnterPhase` armor/speed path is dead code (`check_phase_trigger` hardcodes
  zeros) while the bridge applies stat/ability overrides outside the cascade —
  breaking aura-membership events for phase changes. The doc's own fix (thread the
  full override through serialized `PhaseEntry`) is right and is the highest-value
  *engine-side* item.
- **Aura recompute trigger is a hardcoded `matches!` list** in `step.rs` — replace
  with a named `effect_changes_aura_membership()` predicate (cheap, prevents silent
  misses).
- AI-pipeline internals have their own actively-managed roadmap
  (`docs/ai/tech-debt.md`, phases 0–4 closed) — that layer is in good hands; nothing
  to add beyond its Phase 5 list.

### Smaller observations

- `crates/combat_engine/src/state.rs` is 3025 lines — second-largest file; if touched
  anyway, `Unit` + `CombatState` + phase logic could split, but nothing's wrong with
  it functionally.
- The legality story (one `check_legality`, 3 thin `ActionState` backends, LOS as a
  single trait-default implementation, structural parity + property tests) is a model
  worth replicating — e.g. item 1's AoE enumeration should converge to the same
  pattern.

---

## Suggested order

1. Delete `ai/sim/` + fix ai.md (half a day, pure win, zero behavioral risk).
2. Split `engine_bridge.rs` into a module (1 day, mechanical).
3. Retire `UnitSnapshot` in favor of `UnitView` (1–2 days, golden-replay-guarded).
4. Content dedup: shared engine-side TOML wire structs + rename app `ContentView`
   (2–3 days).
5. Engine-side: phase overrides through `PhaseEntry`/`EnterPhase` + aura predicate
   (per bridge.md §10's own plan).
