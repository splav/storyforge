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
              │  translate_* helpers             │
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
| `process_action_system` | `CombatStep::Execute`, first in chain | Consume `ActionInput` messages, call `step()`, translate events |
| `project_state_to_ecs` | `CombatStep::Execute`, after `process_action_system` | Write engine state back to ECS components (D6 contract) |
| `apply_phase_transitions_system` | `CombatStep::Execute`, after `project_state_to_ecs` | Apply Bevy-only phase deltas (Name, Abilities, AxisProfile) from `Event::PhaseEntered` |

### Event translators (called from process_action_system)

| Function | Translates |
|----------|-----------|
| `translate_move_events` | `Event::Move`, `Event::AoO` → move animations + AoO popups |
| `translate_cast_events` | `Event::Damage/Heal/StatusApplied/…` → damage flashes, status icons, popups |
| `translate_end_turn_events` | `Event::TurnEnded/TurnStarted/RoundStarted/PhaseEntered` → turn-card refresh |
| `translate_tick_events` | Status tick events (called from `bootstrap_combat_state` for round-start priming) |

Translators do NOT write engine-projected components. They produce
`CombatLog` string events, `AnimationQueue` items, and popup entries for UI
consumption.

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
scenario's status definitions.

For the offline equivalent see `TomlContentView` in
`crates/combat_engine/src/toml_content_view.rs`.

---

## 6. Legality tooltips

`src/combat/legality_adapter.rs` wires `combat_engine::check_legality`
against live ECS queries to power UI tooltips ("why can't I use this?").
The engine returns `Result<LegalAction, IllegalReason>`; the adapter
formats the reason for the ability panel.
