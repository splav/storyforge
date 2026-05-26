# Combat Bridge — engine_bridge.rs

`src/combat/engine_bridge.rs` (~1815 lines) is the single file that talks to
both Bevy ECS and the pure engine. Nothing else may call `step()` directly
from the game side (only the AI sim does, via `plan/sim.rs`).

For the engine internals (`step`, `CombatState`, `ContentView`, determinism),
see [`engine.md`](engine.md).
For the system schedule and turn chains, see [`pipeline.md`](pipeline.md).
For combat start/teardown/restart flows, see [`lifecycle.md`](lifecycle.md).

---

## 1. Layer overview

```
              ┌─────────────────────────────────┐
              │           UI / Input            │
              │  src/ui/, src/combat/command_input.rs
              └──────────────┬──────────────────┘
                             │ ActionInput messages
                             │ (Move / Cast / EndTurn)
                             ▼
              ┌─────────────────────────────────┐
              │  Bridge — engine_bridge.rs       │
              │  process_action_system           │
              │  project_state_to_ecs            │
              │  translate_events / translate_one│
              │  bootstrap_combat_state          │
              └──┬──────────────┬──────────┬────┘
     step(state, │              │ events   │ projection
     action, rng,│              ▼          ▼
     content)    │         CombatLog   ECS components
                 ▼         (UI events) (Vital/AP/...)
     ┌───────────────────────┐
     │     Engine (pure)     │
     │  crates/combat_engine/│
     └───────────────────────┘
                 ▲
                 │ same step() API
                 ▼
     ┌───────────────────────┐
     │   AI — src/combat/ai  │
     │  plan/sim.rs          │
     └───────────────────────┘
```

**Three concerns:**
1. **Engine** — pure Rust, no Bevy, authoritative state.
2. **Bridge** — the only place that touches both sides.
3. **Consumers** — AI and UI read projections; they never write engine-owned data.

---

## 2. System table

### Lifecycle

| System | Schedule | Role |
|--------|----------|------|
| `bootstrap_combat_state` | `CombatPhase::StartRound` chain, after `build_turn_order` | Seed `CombatStateRes` from ECS — **once per encounter** (idempotent via `units().is_empty()` guard) |
| `reset_engine_mirrors_on_exit_combat` | `OnExit(AppState::Combat)` | Clear `CombatStateRes` / `UnitIdMap` / `PendingPhaseTransitions` on Victory/Defeat |
| `reset_engine_mirrors_on_restart` | `Update`, reads `RestartCombat` | Same clear, fires on in-place restart without AppState transition |

### Action processing

| System | Schedule | Role |
|--------|----------|------|
| `process_action_system` | `CombatStep::Execute`, first in chain | Consume `ActionInput` messages, call `step()`, translate events into `BridgeQueues` |
| `apply_bridge_queues_pre_projection` | `CombatStep::Execute`, after `process_action_system` | Drain deaths + turn-lifecycle queues (Dead marker, ActiveCombatant) |
| `project_state_to_ecs` | `CombatStep::Execute`, after pre-projection apply | Write engine state back to ECS components (D6 contract) |
| `apply_bridge_queues_post_projection` | `CombatStep::Execute`, after `project_state_to_ecs` | Drain animations + phase queues; `apply_phase_ecs_writes` for `PhaseEntered` (Name, Abilities, AxisProfile) |

### Event translation (called from process_action_system)

Post-Phase A, translation is unified into two functions:

| Function | Role |
|----------|------|
| `translate_events(events, &mut ctx)` | Iterates `Vec<Event>`; calls `translate_one` for each |
| `translate_one(event, &mut ctx)` | Single exhaustive `match` over all `Event` variants (lines 809–1073) |

`TranslateCtx<'a>` bundles all mutable output sinks:
- `log: &mut CombatLog` — text entries for the combat log
- `id_map: &UnitIdMap` — engine ↔ ECS entity mapping
- `queues: &mut BridgeQueues` — deferred side-effect queues
- `cast: CastCtx` — per-cast accumulator (ability, caster entity)
- `move_: MoveCtx` — per-move accumulator (path, entity)

The four former functions (`translate_move_events`, `translate_cast_events`,
`translate_end_turn_events`, `translate_tick_events`) were collapsed into
`translate_one` in Phase A (`a7048f6`).

Translators do NOT write engine-projected components. They produce
`CombatLog` entries, populate `BridgeQueues`, and build animation/popup data
for UI consumption.

### BridgeQueues

`BridgeQueues` (Resource, post-Commit 1 consolidation `505ffa7`) groups the four
formerly-separate `Pending*` Resources into one:

| Sub-field | Drained by | Contents |
|-----------|-----------|---------|
| `deaths: Vec<UnitId>` | `apply_bridge_queues_pre_projection` | Units to mark `Dead` |
| `turn_lifecycle: BridgeTurnLifecycle` | `apply_bridge_queues_pre_projection` | `ActiveCombatant` inserts/removes + round-start flag |
| `animations: Vec<PendingAnim>` | `apply_bridge_queues_post_projection` | Movement animations → `AnimationQueue` |
| `phases: Vec<(UnitId, usize)>` | `apply_bridge_queues_post_projection` | Phase-transition pairs → `apply_phase_ecs_writes` |

Pre-projection queues fire before `project_state_to_ecs`; post-projection
queues fire after, so Vital/AP components are fresh when animations read them.

---

## 3. Turn lifecycle ownership

Engine `step(EndTurn)` is the **sole authority** for turn transitions. The
cascade emits `TurnEnded → [TurnSkipped]* → TurnStarted` for the next actor,
then calls `start_actor_turn` for the new actor and folds its events (AP/MP
refill, mana/energy regen, status ticks) into the same stream.

When an actor dies mid-action (e.g. AoO kills the mover), `Effect::Death` of
the turn-holder derives `Effect::AdvanceTurn` automatically — involuntary turn
end is handled by the engine, no bridge workaround needed.

`CombatStep::TurnStart` set is empty (kept as a stable hook for future
bridge-side systems). All turn-lifecycle events flow through
`process_action_system → project_state_to_ecs` in a single frame — ECS is
always in sync at frame boundary.

Two bridge-side systems deleted in V3:
- `engine_turn_start_system` — turn-start refill now flows through the engine
  cascade.
- `engine_start_first_turn_system` — round-1 first-actor priming is folded
  into `bootstrap_combat_state`.

---

## 4. ECS-projected components (D6 contract)

`project_state_to_ecs` writes these components — and **only
`engine_bridge.rs`** may write them (enforced by
`tests/projection_isolation.rs`):

| Component | Field(s) written |
|-----------|-----------------|
| `HexPositions` | the full position map |
| `Vital` | `.hp` |
| `ActionPoints` | `.action_points`, `.movement_points` |
| `Reactions` | `.remaining`, `.max` |
| `Rage` | `.current` |
| `Mana` | `.current` |
| `Energy` | `.current` |
| `StatusEffects` | `.0` (merges engine-known + ECS-only entries) |
| `BonusMovement` | removed when `movement_points == 0` |

Allowed write exceptions documented in
`tests/projection_isolation.rs::ALLOWED_FILES`:
- `src/combat/turn_order.rs` — `Reactions` write at round start (engine
  refills `reactions_left` via `state.start_round` on round wrap; the ECS
  component is initialized here).
- `src/game/components.rs` — `Vital::apply_damage`/`apply_heal` method impls
  (dead in production after Phase 5; present only for legacy call sites).

---

## 5. EcsContentView

`EcsContentView` (defined in `engine_bridge.rs`) is the live-combat
implementation of the engine's `ContentView` trait. It reads from
`Res<ActiveContent>` for ability and status definitions, and computes real
`StatusBonuses` (including `armor_bonus`, `speed_bonus`) from the active
scenario's status definitions. `status_bonuses` is default-implemented on top
of `status_def` (post-V4); no explicit override is needed.

For the offline equivalent see `TomlContentView` in
`crates/combat_engine/src/toml_content_view.rs`.

---

## 6. Content adapter — `src/content/to_engine.rs`

Bevy content → engine type conversions were consolidated into
`src/content/to_engine.rs` in Phase B-α/β (`0813083`, `12e2fd8`). Key helpers:

| Helper | Produces |
|--------|---------|
| `ability_def(…)` | `AbilityDef` from `ActiveContent` ability entry |
| `status_def(…)` | `StatusDef` from `ActiveContent` status entry |
| `crit_fail_outcome(…)` | `CritFailOutcome` from `ActiveContent` |

Bridge is now pure translation; all content-construction logic lives in
`to_engine.rs`.

---

## 7. HexMap façade (two-layer)

`HexPositions` tracks alive units only. `HexCorpses` tracks dead units'
last positions. The `HexMap` façade composes both for queries that need
the full picture (e.g. pathfinding obstacle checks, AoO range).

- `HexCorpses.generation` is tracked by `ui_dirty_bridge` for UI invalidation
  (C-corpse / C-ui).

---

## 8. Legality tooltips

`src/combat/legality_adapter.rs` wires `combat_engine::check_legality`
against live ECS queries to power UI tooltips ("why can't I use this?").
The engine returns `Result<LegalAction, IllegalReason>`; the adapter
formats the reason for the ability panel.

---

## 9. By-design surface (NOT debt)

These items live in the bridge by design and should not reappear in "what's left" surveys.
See also `engine-migration.md §5` for the authoritative list.

| Item | Why it stays |
|------|-------------|
| `spawn_ecs_entity_from_engine_unit` in `process_action_system` | id_map insertion must be atomic with `UnitSpawned` event — a separate system creates a lookup race |
| Engine-trace writer | Records `(Action, &[Event], rng_calls, hash)`; `Action` is not in the event stream |
| `EcsContentView` | Bridge boundary: wires Bevy queries to the engine's `ContentView` trait |
| `from_ecs` heavy bootstrap mapper | Engine must not know about ECS `Equipment`/`CombatStats`/`EnemyPhases`; mapping is correct in shape |
