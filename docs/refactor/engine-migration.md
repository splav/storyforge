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
| **S7** | `Event::EnergySpent` parity with `EnergyRegenerated` | ✅ Done | subsumed by C4 — `PoolChanged{pool: Energy, cause: Spent}` |
| **V4** | Engine internal: unify two status-bonus reflow paths | ✅ Done | `097f78f` |
| **Phase B** | engine-truth invariant completion (B-α adapter relocation, B-β template consolidation in `src/content/to_engine.rs`, B-γ S6) | ✅ Done | `4b4b0e3` |
| **Phase C** | Resource-pool uniformity: `PoolKind` enum, `Unit.pools` (`EnumMap<PoolKind, Option<(i32,i32)>>`), unified regen loop, `Event::PoolChanged` surface; bridge projector reads from pools (C5); legacy `Unit` fields + dual-emit removed (C6); subsumes S7 | ✅ Done | C1:`cb6bcbc` C2:`ca66039` C3:`d70958b` C4:`c4eca57` C5:`664fbab` C6:_pending_ |

---

## 3. Legacy cleanup

(All legacy cleanup complete — see history.)

L1 and L2 closed in `ffe7e97`.

---

## 4. Deferred engine extensions

No deferred engine extensions remain — all S-items are closed. See
Section 4b for structural improvements landed.

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
| C4: `Event::PoolChanged` + S7 subsumption | `c4eca57` | Unified pool-mutation event surface. Dual-emitted alongside legacy events. AP/MP refill now emits `PoolChanged{Refill}` (previously silent). S7 (`EnergySpent`) subsumed: energy spend is `PoolChanged{pool: Energy, cause: Spent}`. SCHEMA 40→41. |
| **Phase C complete** (C5): bridge reads from `Unit.pools` | `664fbab` | `project_state_to_ecs` sources AP/MP/Rage/Mana/Energy values from `unit.pools[PoolKind::*]`. Legacy fields write-only until C6 removes them. Two bridge_smoke tests updated to keep `pools` in sync with direct legacy-field mutations. |

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

## 6. Completed phases log

All planned migration phases are now closed.

| Phase | Closed | Summary |
|---|---|---|
| L-items (L1/L2) | `ffe7e97` | Dropped dead shim; moved sim helpers |
| S5/S6/S7 | `5db559d` / `4b4b0e3` / C4 | Atomic DotDamaged; auto-end in engine; EnergySpent via PoolChanged |
| Phase A | `a7048f6` | `translate_events` + `TranslateCtx` unification |
| Phase B (B-α/β/γ) | `4b4b0e3` | Engine-truth invariant; adapters to `to_engine.rs` |
| Phase C (C1–C5) | `664fbab` (C5) | Resource-pool table; bridge projector reads from pools |

**Remaining open work (separate sessions):**
- **C6:** Remove legacy fields (`Unit.mana`, `Unit.rage`, `Unit.energy`, `Unit.action_points`, `Unit.movement_points`, `Unit.max_ap`) and legacy events (`ManaRegenerated`, `EnergyRegenerated`, `RageGained`). Fields are currently write-only. Safe to remove once all callers migrate.
- **`Messages<CombatEvent>` conversion** — touches ~6 UI consumers, changes log persistence model. Defer until i18n or AI replay UI forces it.

---

## 7. Done-when — ALL CRITERIA MET ✅

Migration is **complete** as of Phase C-5.

- All L items closed. ✅ (`ffe7e97`)
- At least one of S5/S6/S7 triggered + landed. ✅ S5 in `5db559d`, S6 in `4b4b0e3`, S7 subsumed in C4.
- This document's "Pending" rows are empty. ✅
- `docs/engine-architecture.md` — the file is a redirect stub pointing to `docs/combat/`; the relevant content is in `docs/combat/engine.md` (Unit struct updated) and `docs/combat/bridge.md`. ✅

**The engine migration arc is closed.** The engine is authoritative for combat state. The bridge is a pure projection and translation layer. Resource pools are unified under `Unit.pools[PoolKind]`. All planned phases (A, B, C) are complete.

Open maintenance items (C6, `Messages<CombatEvent>`) are tracked in Section 6 and are not blocking — they are cosmetic legacy cleanup.
