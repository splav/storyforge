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
| **L1** | Drop empty `advance_turn_system` shim | ✅ Done | `ffe7e97` |
| **L2** | Move `effects_outcome.rs` + `effects_state.rs` → `src/combat/ai/sim/` | ✅ Done | `ffe7e97` |
| **S5** | `Event::DotDamaged` atomic (drop bridge `pending_status_tick` state-machine) | ✅ Done | `5db559d` |
| **Phase A** | Unify bridge translators into `translate_events` + `TranslateCtx` | ✅ Done | `a7048f6` |
| **S6** | auto-end-turn lives in engine, not bridge | ✅ Done | `4b4b0e3` |
| **S7** | `Event::EnergySpent` parity with `EnergyRegenerated` | ⏳ Deferred | wait for trigger |
| **V4** | Engine internal: unify two status-bonus reflow paths | ✅ Done | `097f78f` |
| **Phase B** | engine-truth invariant completion (B-α adapter relocation, B-β template consolidation in `src/content/to_engine.rs`, B-γ S6) | ✅ Done | `4b4b0e3` |

---

## 3. Legacy cleanup

(All legacy cleanup complete — see history.)

L1 and L2 closed in `ffe7e97`.

---

## 4. Deferred engine extensions

All require an engine SCHEMA bump and parity-test churn. If two or
more trigger simultaneously, group into a single bump window.

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

### V4 — engine status-bonus reflow unification ✅ Done (`097f78f`)

Extended `StatusBonuses` to carry all three bonus fields
(`armor_bonus`, `speed_bonus`, `damage_taken_bonus`). Collapsed the
flat bonus fields on `StatusDef` into `bonuses: StatusBonuses`.
`ContentView::status_bonuses` is now default-implemented on top of
`status_def`. `RefreshAggregates` and `aura_effects_on` read all
bonuses through a single call. Production overrides on
`TomlContentView` and `EcsContentView` deleted. Also closed a silent
AI-sim divergence: `SnapshotContentView` previously returned only
armor+speed without `damage_taken_bonus`. SCHEMA unchanged (hashes
`Unit` aggregates, not `StatusDef`). 44 files, −105 net lines.

---

## 4b. Recent structural improvements

| Change | Commit | What it does |
|---|---|---|
| S5: `Event::DotDamaged` | `5db559d` | Fused `StatusTicked + UnitDamaged` pair into one atomic event; dropped `pending_status_tick` state-machine from bridge. |
| Phase A: translator unification | `a7048f6` | Collapsed `translate_tick_events`, `translate_end_turn_events`, `translate_cast_events`, `translate_move_events` into `translate_events(events, &mut TranslateCtx)` with one exhaustive `match`. |
| Phase B-α: adapter relocation | `0813083` | Moved content-adapter helpers out of bridge into `src/content/to_engine.rs`; bridge is now pure translation. |
| Phase B-β: template consolidation | `12e2fd8` | Consolidated remaining bridge-side engine-construction templates into `src/content/to_engine.rs`. |
| S6 / Phase B-γ: auto-end-turn in engine | `4b4b0e3` | `Event::TurnEnded{cause: ResourcesExhausted}` emitted inline by Cast arm; bridge auto-end block removed. Closed engine-truth invariant. |

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

**Engine extensions (wait for trigger):**
- S7 individually scoped (only remaining deferred engine extension).

**Cross-cutting (separate scope):**
- `Messages<CombatEvent>` conversion — would touch ~6 UI consumers,
  fundamentally changes log persistence model. Defer until i18n or AI
  replay UI forces it.

---

## 7. Done-when

Migration is **complete** when:

- All L items closed. ✅ (`ffe7e97`)
- At least one of S5/S6/S7 triggered + landed (proves engine schema
  evolution path works post-PR-A). ✅ S5 landed in `5db559d`, S6 landed
  in `4b4b0e3`.
- This document's "Pending" rows are empty or moved to historical record.
- `docs/engine-architecture.md` updated to reflect final post-migration
  shape.

**Migration is essentially complete** — Phase B (B-α/B-β/B-γ) closed the
engine-truth invariant. Only S7 (`Event::EnergySpent`) remains as a
deferred-pending-trigger item. The schema evolution path is proven.

No hard deadline — migration is opportunistic, driven by triggers and
session bandwidth.
