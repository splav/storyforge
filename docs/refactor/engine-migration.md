# Engine Migration Status

Single source of truth for ECS↔engine migration debt. Supersedes the
"what's pending" sections of:

- [`history/combat-event-mirror.md`](history/combat-event-mirror.md) (PR-A/B done, stages 5-7 tracked here)
- [`history/engine-bootstrap-v3.md`](history/engine-bootstrap-v3.md) (V1-V3 done, V4 tracked here)

Those documents remain as **historical record** of decisions made; this
file is **current state**. Test-helper refactoring lives separately in
`helpers-normalization-plan.md` — different concern.

---

## 1. Mental model

The migration moves **combat truth** from ECS systems to the
`combat_engine` crate. Bridge (`src/combat/engine_bridge.rs`) translates
between the two:

- **Engine** owns `CombatState`, runs `step(Action)` → `Vec<Event>`.
- **Projector** writes engine state back to ECS components every frame.
- **Translate-functions** convert engine `Event` → `CombatLog` entries
  and push to queue-Resources.
- **Apply-systems** drain the queues into ECS side-effects (Dead,
  ActiveCombatant, AnimationQueue, NextState).

What stays in ECS **by design** (not debt):

- `spawn_ecs_entity_from_engine_unit` in `process_action_system` —
  synchronous summon, id_map race risk if extracted.
- Engine-trace writer — needs the original `Action`, which `Event` does
  not carry.
- `EcsContentView` — engine's `ContentView` trait implementation backed
  by Bevy queries. Bridge boundary, not duplication.
- `from_ecs` heavy bootstrap mapper — engine doesn't (and shouldn't)
  know about ECS `Equipment` / `CombatStats` / `EnemyPhases` /
  `AuraSource` shapes. Mapping concentrated in one function is correct.

---

## 2. Status table

| ID | Item | Status | Commit / Issue |
|---|---|---|---|
| V1 | `EcsContentView::status_bonuses` stub | ✅ Done | pre-session |
| V3 | bootstrap-once engine lifecycle | ✅ Done | `6a4f24b` + `9c08d60` |
| PR-A | `CombatEvent` flat mirror of engine `Event` + mana-event slice + Localizer hook | ✅ Done | `09648b6` |
| C-corpse | Two-layer hex map (HexCorpses + HexMap façade) | ✅ Done | `264ac9b` |
| C-ui | `ui_dirty_bridge` tracks `HexCorpses.generation` | ✅ Done | `df33a9a` |
| C-dead | Unified deadness predicate (`vital.hp ≤ 0`) + corpse stationarity assert | ✅ Done | `8102aa1` |
| PR-B | Bridge side-effects → apply-systems | ✅ Done | `7914276` |
| **L1** | Drop empty `advance_turn_system` shim | ⏳ Pending | trivial cleanup |
| **L2** | Move `effects_outcome.rs` + `effects_state.rs` → `src/combat/ai/sim/` | ⏳ Pending | naming fix |
| **S5** | `Event::DotDamaged` atomic (drop bridge `pending_status_tick` state-machine) | ⏳ Deferred | wait for trigger |
| **S6** | auto-end-turn lives in engine, not bridge | ⏳ Deferred | wait for trigger |
| **S7** | `Event::EnergySpent` parity with `EnergyRegenerated` | ⏳ Deferred | wait for trigger |
| **V4** | Engine internal: unify two status-bonus reflow paths | ⏳ Deferred | wait for trigger |

---

## 3. Legacy cleanup — pending

### L1 — drop `advance_turn_system` empty shim

[src/combat/advance_turn.rs:22](../../src/combat/advance_turn.rs:22) is
`pub fn advance_turn_system() { /* empty */ }`, kept only as "stable
registration point" for `check_victory_system`. Same effect via
`SystemSet` ordering. ~10 LOC removal, zero risk.

### L2 — relocate AI sim files

`src/combat/effects_outcome.rs` and `src/combat/effects_state.rs` are
the AI's predictive simulation (`compute_ability_outcome`,
`compute_affected_targets`) — parallel to engine `step()` for fast
scoring rollouts. They live in `src/combat/` but semantically belong
under `src/combat/ai/sim/`.

Move + update callsites (`src/combat/ai/plan/sim.rs`,
`src/combat/ai/plan/parity_tests.rs`, `src/combat/ai/scoring/factors/aoe_hits.rs`).
Parity-tests against engine `targeting::compute_affected_targets` and
`step()` continue to guarantee divergence detection.

~5 minutes of `mv` + import updates.

---

## 4. Deferred engine extensions

All four require an engine SCHEMA bump and parity-test churn. If two or
more trigger simultaneously, group into a single bump window.

### S5 — `Event::DotDamaged` atomic

**Problem.** Engine currently emits a pair: `Event::StatusTicked` then
`Event::UnitDamaged` for each DoT proc. Bridge ties them via a local
state-machine `pending_status_tick: Option<(UnitId, StatusId)>` in
[engine_bridge.rs:645](../../src/combat/engine_bridge.rs:645). The
pairing relies on documented event order — fragile if a future event
ever slips between them.

**Fix.** Engine emits one `Event::DotDamaged { target, source_status, raw, mitigation, pierces, amount }`.
Drops the state-machine in bridge. SCHEMA 39→40.

**Trigger.** A third DoT variant lands, or a bridge bug in pairing
surfaces. Today the pair is well-behaved.

### S6 — auto-end-turn in engine

**Problem.** When a Cast exhausts `AP=0 && MP=0`, the bridge
([engine_bridge.rs:917](../../src/combat/engine_bridge.rs:917))
synchronously calls `step(Action::EndTurn)` after the Cast. This means
a pure engine replay (without the bridge) would not auto-end — the
trace records two separate steps but the chain of cause is invisible.
Also: AI's agenda-builder has to know about this implicit
end-on-exhaustion behavior.

**Fix.** Engine `Action::Cast` arm itself emits the AdvanceTurn cascade
when actor resources are depleted. SCHEMA 40→41. Drop the bridge's
auto-end block.

**Trigger.** AI agenda-builder grows logic depending on auto-end, or
replay fidelity matters for a debugging task. **Highest risk** of the
stages because it changes the AI surface.

### S7 — `Event::EnergySpent`

**Problem.** Mana has both `ManaRegenerated` (engine) and the PR-A
mini-slice that emits `ManaRegenerated` on `PayCost{Mana}`. Energy has
only `EnergyRegenerated` — energy spend is currently captured by
bridge-side diff in `process_action_system`. Asymmetric.

**Fix.** Engine emits `Event::EnergySpent` from
`Effect::PayCost { kind: Energy }`. Bridge drops the diff path. SCHEMA
41→42.

**Trigger.** Energy is spent outside `Action::Cast` (toggle aura,
passive drain, ability-of-another-unit consuming target's energy).
Today the diff path captures everything.

### V4 — engine status-bonus reflow unification

**Problem.** Engine pulls `armor_bonus`/`speed_bonus` from
`ContentView::status_bonuses(status_id)` and `damage_taken_bonus` from
`status_def.damage_taken_bonus` — two paths, two sources of truth.
Synced today by convention, brittle.

**Fix.** Pure engine internal: collapse both into a single
`status_def.bonuses() -> StatusBonuses { armor, speed, damage_taken }`
or extend `StatusBonuses` to carry all three. SCHEMA may not need to
bump (depends on TOML shape).

**Trigger.** Adding a fourth bonus type, or a status-bonus bug at the
reflow boundary.

---

## 5. By-design surface (NOT debt)

Listed here so they don't reappear in future "what's left" surveys.

| Item | Where | Why it stays in bridge / ECS |
|---|---|---|
| `spawn_ecs_entity_from_engine_unit` | `process_action_system` | id_map insertion is atomic with engine `UnitSpawned` event — extracting to a separate system creates a race for any subsequent event referencing the new uid. |
| Engine-trace writer | `process_action_system` lines 791/857/909 | Trace records `(Action, &[Event], rng_calls, hash)`. `Action` isn't in `Event` stream — must live where dispatch happens. |
| `EcsContentView` per-step | `engine_bridge.rs:264-298` | Engine's `ContentView` trait is the right abstraction. Bridge wires Bevy queries to it. Performance is fine; small combats. |
| `from_ecs` heavy mapping | `engine_bridge.rs:175-280` | Engine should not know about ECS Equipment/CombatStats. Mapping is correct in shape; size reflects real domain complexity. |
| `CombatLog` as `Resource(Vec<...>)`, not `Messages<...>` | `src/game/combat_log.rs` | `CombatStarted`/`CombatEnded` need persistence; many UI consumers (popup, log_ui, console_log, AI memory) read full history. Converting to Messages is a separate refactor (~"PR-A.5"), not currently justified. |

---

## 6. Suggested sequencing

**Cheap wins (any time):**
- L1 + L2 together (~30 min, low risk). Pure cleanup, no functional change.

**Engine extensions (wait for trigger):**
- S5 / S6 / S7 individually scoped, group if triggers coincide.
- V4 is pure engine internal; can land with any of S5-S7.

**Cross-cutting (separate scope):**
- `Messages<CombatEvent>` conversion — would touch ~6 UI consumers,
  fundamentally changes log persistence model. Defer until i18n or AI
  replay UI forces it.

---

## 7. Done-when

Migration is **complete** when:

- All L items closed.
- At least one of S5/S6/S7 triggered + landed (proves engine schema
  evolution path works post-PR-A).
- This document's "Pending" rows are empty or moved to historical record.
- `docs/engine-architecture.md` updated to reflect final post-migration
  shape.

No hard deadline — migration is opportunistic, driven by triggers and
session bandwidth.
