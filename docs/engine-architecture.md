# Combat Engine Architecture (post-unisim, Phases 0–6)

This document describes the runtime architecture after the `ai/unisim` migration
closed (`unisim/phase6-complete` tag, 2026-05-18). It supersedes the
combat-pipeline / module-map sections of [architecture.md](architecture.md) and
[combat-pipeline.md](combat-pipeline.md) wherever they conflict.

For the migration history and design decisions, see
[`docs/ai/rework/unisim.md`](ai/rework/unisim.md) and the per-phase plans
(`step_unisim*.md`).

---

## 1. Layer overview

```
                ┌──────────────────────────────────────┐
                │              UI / Input              │
                │  src/ui/, src/combat/command_input.rs│
                └────────────────┬─────────────────────┘
                                 │ ActionInput messages
                                 │ (Move / Cast / EndTurn)
                                 ▼
                ┌──────────────────────────────────────┐
                │   Bridge — src/combat/engine_bridge  │
                │  process_action_system,              │
                │  project_state_to_ecs,               │
                │  translate_*_events,                 │
                │  init_state_from_ecs                 │
                └──┬───────────────────┬──────────┬────┘
       step(state, │                   │ events   │ projection
       action, rng,│                   ▼          ▼
       content) ─► │             CombatLog    ECS components
                   │             (UI events) (Vital/AP/...)
                   ▼
        ┌──────────────────────────┐
        │      Engine (pure)       │
        │ crates/combat_engine/    │
        │  state.rs / step.rs      │
        │  effect.rs / event.rs    │
        │  reaction.rs (AoO)       │
        │  targeting.rs            │
        │  trace.rs                │
        └──────────────────────────┘
                   ▲
                   │ same step() API
                   │
        ┌──────────────────────────┐
        │     AI — src/combat/ai   │
        │  plan/sim.rs (uses step)│
        │  scoring/, intent/      │
        │  outcome/, repair/      │
        │  log/ (JSONL)           │
        └──────────────────────────┘
```

**Three distinct concerns:**

1. **Engine** (`crates/combat_engine/`) — pure Rust, no Bevy, deterministic.
   Owns canonical state mutations: damage, healing, status apply/tick, AoO,
   phase transitions, auras, turn queue, end-turn, move.
2. **Bridge** (`src/combat/engine_bridge.rs`) — the only place that talks to
   both sides. Reads ECS, calls `step()`, projects state back, translates
   events for UI/animation.
3. **Consumers** — AI plans actions (calling the same `step()` internally for
   evaluation); UI dispatches input as `ActionInput` messages and reads the
   ECS projection for rendering.

---

## 2. Engine

### 2.1 Public API

```rust
pub fn step(
    state:  &mut CombatState,
    action: Action,
    rng:    &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<(Vec<Event>, ApplyCtx), ActionError>;
```

- **`CombatState`** carries everything the engine needs: `units: Vec<Unit>`,
  `round: u32`, `phase: RoundPhase`, `turn_queue: TurnQueue`,
  `random_seed: u64`, `next_synthetic_uid: u64`. Cloneable for sim rollback
  (decision 6.5: strict failure, state rollback on error).
- **`Action`** is `Move { actor, path }` | `Cast { actor, ability, target, target_pos }`
  | `EndTurn { actor }`. Serde-derived (5a).
- **`Event`** is the observable consequence stream (`Damage`, `Heal`, `Died`,
  `StatusApplied`, `StatusTicked`, `AoO`, `Move`, `TurnStarted`, `TurnEnded`,
  `RoundStarted`, `PhaseEntered`, etc.). Also serde-derived.
- **`ApplyCtx`** carries `rng_calls: u64` (Phase 5 D2 — per-step RNG canary
  for replay drift detection).

### 2.2 ContentView trait (4 methods after 5c.1)

The trait was deliberately contracted from 8 methods to 4 — anything that's
per-combat-instance moved into `Unit` fields:

```rust
pub trait ContentView {
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef>;
    fn status_def(&self, id: &StatusId) -> Option<StatusDef>;
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses;
    fn unit_template(&self, id: &str) -> Option<UnitTemplate>;
}
```

Two implementations:
- **`EcsContentView`** (bridge-side, in `engine_bridge.rs`) — reads from
  `Res<ActiveContent>` for live combat.
- **`TomlContentView`** (engine-side, in `crates/combat_engine/src/toml_content_view.rs`) —
  Bevy-free, parses `assets/data/*.toml` directly. Used by
  `replay_engine_trace` and other offline tools.

Per-combat data (auras, enemy phase definitions, weapon dice for AoO,
caster context) lives on `Unit` directly:

```rust
pub struct Unit {
    pub id: UnitId, pub team: Team, pub pos: Hex,
    pub hp: i32, pub max_hp: i32, pub armor: i32, pub armor_bonus: i32,
    pub action_points: i32, pub movement_points: i32, pub speed: i32,
    pub reactions_left: u32, pub max_reactions: u32,
    pub rage: Option<(i32, i32)>, pub mana: Option<(i32, i32)>, pub energy: Option<(i32, i32)>,
    pub statuses: Vec<ActiveStatus>,
    pub caster_context: CasterContext,        // 5c.1 — equipment/stats for damage
    pub auras: Vec<AuraDef>,                  // 5c.1 — auras this unit emits
    pub enemy_phases: Vec<PhaseEntry>,        // 5c.1 — phase triggers if boss
    pub aoo_dice: Option<DiceExpr>,           // 5c.1 — weapon dice for AoO
}
```

Init: `init_state_from_ecs` (bridge) reads ECS components and fills all these
fields **once per combat session** (gated on `ctx.round < 2` — fires on round 1
production entry and on round 0 test-harness entry; round 2+ is skipped because
engine state evolves authoritatively via `step()` cascade from there). On
combat end (`OnExit(AppState::Combat)`) `reset_engine_mirrors_on_exit_combat`
clears `CombatStateRes` / `UnitIdMap` / `PendingPhaseTransitions`.

### 2.3 Determinism contract (Phase 5)

- **`DiceRng`** is seeded once per combat from `CombatStateRes.random_seed`
  (D2). Engine calls only `roll_d()`, never `SystemTime::now()` or any
  non-deterministic source.
- **`engine_purity.rs` test** greps `crates/combat_engine/src/**/*.rs` for
  forbidden imports (`std::time::{SystemTime, Instant}`, `std::env`,
  `std::process`, `thread_local!`) — zero finds.
- **`aura_membership_set: BTreeSet`** (5a, was HashSet) — the only known
  iteration-order risk in the engine.
- **`post_state_hash(state)`** = BLAKE3 over canonical serialization of
  `{round, phase, turn_queue, alive_units sorted by id}`. Written as a canary
  in every StepLine; replay compares hash per step to localize drift mid-trace.

---

## 3. Bridge

`src/combat/engine_bridge.rs` is the only Bevy code that talks to the engine
directly. ~1614 lines, ~12 system / function entries:

| Entry | Schedule | Role |
|---|---|---|
| `init_state_from_ecs` | `OnEnter(CombatPhase::AwaitCommand)` | Seed `CombatStateRes` from ECS **once per combat** (`ctx.round < 2` guard) |
| `engine_start_first_turn_system` | `OnEnter(CombatPhase::AwaitCommand)` chained after init | Call `state.start_actor_turn()` for round-1 first actor only (`ctx.round == 1` guard) |
| `process_action_system` | `Update`, gated by `AwaitCommand`, after AI tick | Consume `ActionInput` messages, call `step()`, translate events |
| `apply_phase_transitions_system` | `Update`, after `project_state_to_ecs` | Apply Bevy-only deltas from `Event::PhaseEntered` (Name, Abilities, AxisProfile) |
| `project_state_to_ecs` | `Update`, after `process_action_system` | Write engine state back to ECS components |
| `reset_engine_mirrors_on_exit_combat` | `OnExit(AppState::Combat)` | Clear `CombatStateRes` / `UnitIdMap` / `PendingPhaseTransitions` on combat end |
| `reset_engine_mirrors_on_restart` | `Update`, reads `RestartCombat` | Same clear, fires on in-place restart (no AppState transition) |

### 3.0 Turn lifecycle ownership (post-Phase-1+B5 bridge work, 2026-05-21)

Engine `step(EndTurn)` is the sole authority for turn transitions. The cascade
emits `TurnEnded` → `[TurnSkipped]*` → `TurnStarted` for the next actor, then
**also calls `start_actor_turn` for the new actor** and folds its events
(AP/MP refill, mana/energy regen, status ticks) into the same stream.

When an actor dies mid-action (e.g. AoO kills the mover), `Effect::Death`
of the current turn-holder derives `Effect::AdvanceTurn` automatically —
involuntary turn end is handled by the engine, no bridge-side workaround.

`CombatStep::TurnStart` set is empty (kept as stable hook for future systems).
The previous `engine_turn_start_system` was deleted because its job is now
in the engine cascade. All turn-lifecycle events flow through
`process_action_system → project_state_to_ecs` in a single frame — ECS is
always in sync at frame boundary.

### 3.1 ECS-projected components (D6 contract)

`project_state_to_ecs` writes these components (and **only**
`engine_bridge.rs` may write them — enforced by
`tests/projection_isolation.rs`):

- `HexPositions` (the position map)
- `Vital.hp`
- `ActionPoints.{action_points, movement_points}`
- `Reactions.remaining`
- `Rage.current`, `Mana.current`, `Energy.current`
- `StatusEffects.0` (merging engine-known + ECS-only entries)
- `BonusMovement` (removed when `movement_points == 0`)

Allowed write exceptions documented in `tests/projection_isolation.rs::ALLOWED_FILES`:
- `src/combat/turn_order.rs` — round-start reaction refill (legacy, pre-engine seed path)
- `src/game/components.rs` — `Vital::apply_damage`/`apply_heal` method impls (dead in production after Phase 5)

### 3.2 Event translators

After `step()` returns `Vec<Event>`, the bridge translates each event into
`CombatEvent` (UI-facing) + `AnimationQueue` items + `CombatLog` entries:

- `translate_move_events` — `Event::Move`, `Event::AoO` → UI move animations + AoO popups
- `translate_cast_events` — `Event::Damage`/`Heal`/`StatusApplied`/etc. → UI damage flashes, status icons, popups
- `translate_end_turn_events` — `Event::TurnEnded`/`TurnStarted`/`RoundStarted`/`PhaseEntered` → UI turn-card refresh

**Note:** these translators do NOT write engine-projected components.
Their job is to produce *projections* (CombatLog string events, animations,
popup queue items) for UI consumption. The engine remains authoritative.

---

## 4. AI integration

```
  AI system (src/combat/ai/system.rs, Update tick)
    1. start_step = trace_writer.step_counter()       ← Phase 6c
    2. snap = build_snapshot(&world)                  ← read ECS into BattleSnapshot
    3. result = pick_action(&world, &snap, ...)       ← pure Rust, returns AiDecision
    4. Build OwnedActorTickEvent (pre-serialize as serde_json::Value)
    5. Push (value, start_step) → PendingAiLogEntries ← Phase 6c (deferred write)
    6. Dispatch ActionInput messages (Move / Cast / EndTurn)
                                  │
                                  ▼ same Update tick
  process_action_system  ← consumes ActionInput, calls step(), advances counter
                                  │
                                  ▼
  flush_pending_ai_log_system     ← Phase 6c
    1. end_step = trace_writer.step_counter()
    2. For each pending entry: range = (start, next entry's start || end_step)
    3. Patch engine_step_range into JSON, write to ai.jsonl
    4. Clear queue
```

### 4.1 AI sim path

`src/combat/ai/plan/sim.rs::SimState::apply_step` calls
`combat_engine::step::step()` directly to evaluate candidate plans during
beam-search planning. This is the **same engine entry point** the live bridge
uses — guaranteeing that "what AI thinks will happen" matches "what actually
happens" (decision 6.5: zero sim/real drift).

The sim uses a stub-ish `ContentView` adapter (`SnapContent` in
`tests/combat_engine/parity.rs`) and lookalike unit data carried by
`UnitSnapshot`. After Phase 5c.1 the per-combat fields live on `Unit`, so the
sim builds full `Unit` instances rather than relying on content callbacks.

### 4.2 Two log streams per fight

Each combat produces a folder under `logs/<fight_id>/`:

- **`ai.jsonl`** — one `actor_tick` event per AI decision (with full snapshot,
  intent reasoning, scored plans, and `engine_step_range: [start, end)` after
  6c). SCHEMA_VERSION = 36 (additive).
- **`engine.jsonl`** — one `InitLine` (units / round / phase / turn_queue /
  rng_seed / content_hash / session_id) + one `StepLine` per engine
  `step()` call (action / events / rng_calls / post_state_hash).
  SCHEMA_VERSION = 38.

Both files carry `session_id = <fight_id>` (= folder name, D11) so external
tools can join them.

---

## 5. User input

`ActionInput` is the universal message at the bridge boundary — anything
that wants to drive combat (player keyboard, mouse click, AI tick)
dispatches one. The engine never sees Bevy `Entity` IDs; `process_action_system`
maps `Entity → UnitId` via `Res<UnitIdMap>` before constructing `Action`.

### 5.1 Player input

```
  src/combat/command_input.rs (Update tick, gated AwaitCommand)
    - Keys 1-5     → select ability slot, enter targeting mode
    - M            → enter move mode
    - Tab          → cycle targets / hexes in current mode
    - Enter        → confirm; emit ActionInput::{Move,Cast}
    - E            → emit ActionInput::EndTurn
    - Escape       → cancel current mode

  src/ui/hex_grid/input.rs (Update tick)
    - Mouse click on reachable hex → ActionInput::Move
    - Click in cast-targeting mode → ActionInput::Cast
```

### 5.2 Legality tooltips

`src/combat/legality_adapter.rs` wires `crate::combat_engine::check_legality`
against live ECS queries to power UI tooltips ("why can't I use this?"). The
engine returns `Result<LegalAction, IllegalReason>`; the adapter formats the
reason for the panel.

---

## 6. Rendering

The renderer is read-only against the ECS projection — it never mutates
engine-projected components.

| Component / Resource | Written by | Read by |
|---|---|---|
| `HexPosition` | `project_state_to_ecs` | `ui/hex_grid/visuals.rs` (token placement) |
| `Vital.hp` | `project_state_to_ecs` | turn-order panel HP bars, hover tooltip |
| `ActionPoints.{action,movement}_points` | `project_state_to_ecs` | HUD bottom bar, ability panel gating |
| `Reactions.remaining` | `project_state_to_ecs` | turn-order panel "reaction dot" |
| `StatusEffects` | `project_state_to_ecs` | status icon row in turn-order card |
| `CombatLog` (`Res`) | event translators (Update) | `ui/log_ui.rs`, `combat/enemy_popup.rs` |
| `AnimationQueue` | event translators (Update) | `ui/animation.rs` |
| `Res<TurnQueue>` | `combat/turn_order.rs` (round start) | `ui/turn_order_ui.rs`, `ui/hex_grid/visuals.rs` |

The animation queue blocks `CombatPhase` transitions while animations play
(`combat_ready()` predicate). Combat events resolve instantly inside the
engine; UI smoothing happens via `AnimationQueue` interpolation.

### 6.1 UI dirty flags

`Res<UiDirty>` (`game/resources.rs`) is a coarse bitflag set by ECS observers
when relevant ECS components change. UI panels gate their refresh on these
flags — see `ui/combat_ui.rs` for the read patterns.

---

## 7. Replay & determinism path

The engine trace is the system-of-record for "what happened":

```
  Combat starts
    └── open_combat_logs_on_combat_enter
          ├── create logs/<fight_id>/
          ├── open engine.jsonl (always, ungated)
          └── open ai.jsonl (gated by settings.ai_log)
  After init_state_from_ecs (first AwaitCommand)
    └── write_engine_trace_init_system
          └── write InitLine { units, round, phase, turn_queue, rng_seed, content_hash, session_id }
  Each step() call
    └── process_action_system
          └── trace_writer.write_step(action, events, ctx.rng_calls, post_state_hash_hex(&state))
  AI tick (deferred write)
    └── PendingAiLogEntries.push((event, start_step))
  After process_action_system
    └── flush_pending_ai_log_system patches range, writes ai.jsonl
  Combat ends
    └── close_engine_trace_on_combat_exit + close_ai_log_on_combat_exit
```

### 7.1 Replay binaries

- **`cargo run --bin replay_engine_trace -- logs/<fight_id>/engine.jsonl`** —
  rebuilds `CombatState` from `InitLine`, re-seeds `DiceRng`, calls `step()`
  for each `StepLine.action`, asserts byte-equal events + matching rng_calls
  + matching post_state_hash for every step. Flags: `--strict-content` (D3),
  `--tolerance <eps>` (D10).
- **`cargo run --bin replay_ai_log -- logs/<fight_id>/ai.jsonl`** —
  re-runs `pick_action` on each logged `actor_tick` event and reports
  decision changes (regression diagnostic for AI scoring tweaks).

### 7.2 What replay catches

Replay-determinism tests (`crates/combat_engine/tests/replay.rs`, 8 scenarios)
guarantee byte-equal reproduction on the recording host (D10: cross-host is
best-effort + warn). Two divergence sentinels (`replay_event_divergence_detected`,
`replay_rng_count_divergence_detected`) prove the harness catches real drift.

The `projection_isolation.rs` test guards against drift between engine state
and ECS components introduced by accidental ECS writes outside the bridge.

---

## 8. Key invariants (cheat sheet)

- **Engine never sees `bevy::*` types.** Engine `step()` takes `&dyn ContentView`
  + `&mut dyn DiceSource` — both engine-side traits. Bridge converts.
- **ECS components are read-only outside the bridge.** Enforced by
  `tests/projection_isolation.rs` + 3-file allowlist with one-line
  justifications.
- **Sim and live use the same `step()`.** AI planning calls
  `combat_engine::step::step()` directly via `plan/sim.rs::SimState::apply_step`.
  Zero sim/real drift by construction.
- **One folder per combat.** `logs/<timestamp>_<campaign>_<scenario>_<encounter>/`
  contains both streams. Folder name = `session_id`.
- **Schema versioning is per-stream.** AI log SCHEMA_VERSION (currently 36)
  and engine trace SCHEMA_VERSION (currently 38) bump independently.
- **Bench gate: engine ≤ 1.2× legacy.** Phase 6 baseline at 0.88× (engine is
  ~14% faster than the deleted legacy sim path). See `benches/README.md`.

---

## 9. Related docs

- [`docs/architecture.md`](architecture.md) — top-level state machines + module map (may have stale combat refs; this doc is canonical for the engine boundary)
- [`docs/combat-pipeline.md`](combat-pipeline.md) — per-system schedule details
- [`docs/mechanics.md`](mechanics.md) — gameplay rules (stats, damage formula, statuses)
- [`docs/ai/ai.md`](ai/ai.md) — AI architecture (planning, scoring, intent)
- [`docs/ai/replay.md`](ai/replay.md) — AI replay tool usage
- [`docs/ai/rework/unisim.md`](ai/rework/unisim.md) — full migration history + phase retrospectives
- [`docs/ai/extension-checklist.md`](ai/extension-checklist.md) — touch lists for adding new mechanics
- [`benches/README.md`](../benches/README.md) — perf baselines
