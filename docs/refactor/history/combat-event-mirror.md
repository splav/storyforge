# CombatEvent Mirror Refactor

> **Historical record.** PR-A and PR-B (the work described in this plan) are
> complete and committed (`09648b6` and `7914276`). Stages 5/6/7 — the
> engine-side schema bumps deferred from §6 — now live in
> [`engine-migration.md`](../engine-migration.md), which is the current
> source of truth for "what's still pending". This file preserves the
> rationale for the architectural decisions (D1-D8) made during PR-A.

Plan to fix the `[оглушён]`-on-dead-units bug as the surface symptom of a
broader architectural problem: `CombatEvent` (the ECS/UI event enum) and
engine `Event` (the pure-Rust truth source) have drifted apart through
lossy translation in `engine_bridge.rs`.

**Status:**

| Phase | Status | Commit |
|---|---|---|
| Plan agreed (this doc) | ✅ Done | — |
| PR-A "CombatEvent flat mirror" | ✅ Done | `09648b6` |
| PR-B "Side-effect extraction" | ✅ Done | `7914276` |
| Engine schema bumps (etапы 5/6/7) | ⏳ Deferred — see [engine-migration.md](../engine-migration.md) | — |

---

## 1. Problem (concrete bug → systemic class)

### 1.1 Surface bug

UI prints `○ Зверокров Падальщик пропускает ход [оглушён]` for a **dead**
unit, every round, until end of combat. `engine.jsonl` for the same
events shows `{"turn_skipped":{"actor":...,"reason":"dead"}}`.

Root cause is a 3-layer drop of structural information:

1. **Engine** correctly distinguishes — `Event::TurnSkipped { actor, reason: TurnSkipReason }` with `Dead | Stunned` ([crates/combat_engine/src/event.rs:67](../../../crates/combat_engine/src/event.rs:67), :97).
2. **Bridge** drops `reason`:
   ```rust
   // src/combat/engine_bridge.rs:1008
   Event::TurnSkipped { actor, .. } => {
       log.push(CombatEvent::TurnSkipped { actor: ent });
   }
   ```
3. **ECS event** has no `reason` field ([src/game/combat_log.rs:50](../../../src/game/combat_log.rs:50)).
4. **Formatter** hardcodes the string ([src/game/combat_log.rs:148](../../../src/game/combat_log.rs:148)).

### 1.2 Same class of problem — found in 6+ places

The bridge **lossy-translates** structured engine data to ad-hoc strings
or drops fields in at least these places:

| Smell | Where | Class |
|---|---|---|
| `TurnSkipped.reason` dropped | [combat_log.rs:50](../../../src/game/combat_log.rs:50) + [engine_bridge.rs:1008](../../../src/combat/engine_bridge.rs:1008) | this bug |
| `CritFailSideEffect { effect_name: String }` mixes 3 domain entities into one string, loses status localization | [combat_log.rs:97](../../../src/game/combat_log.rs:97) + [engine_bridge.rs:1152](../../../src/combat/engine_bridge.rs:1152) | structured→string |
| `SummonBlocked { reason: String }` — `SpawnBlockedReason` enum converted to RU text in the bridge | [combat_log.rs:119](../../../src/game/combat_log.rs:119) + [engine_bridge.rs:1197](../../../src/combat/engine_bridge.rs:1197) | structured→string |
| `HealResult.formula = "engine".into()` — placeholder value visible to user as `"лечение: engine → +N HP"` | [combat_log.rs:37](../../../src/game/combat_log.rs:37) + [engine_bridge.rs:1117](../../../src/combat/engine_bridge.rs:1117) | dead field leaks |
| `PoisonTick`/`PoisonCleansed` — legacy naming for any DoT (engine emits generic `StatusTicked`) | [combat_log.rs:82](../../../src/game/combat_log.rs:82) + [engine_bridge.rs:649](../../../src/combat/engine_bridge.rs:649) | legacy naming |
| `ManaChanged` derived from `mana_before`/`mana_after` snapshot diff instead of an engine fact | [engine_bridge.rs:1245](../../../src/combat/engine_bridge.rs:1245) | missing engine event |
| `WillOverload` — variant never `push`-ed anywhere | [combat_log.rs:101](../../../src/game/combat_log.rs:101) | dead variant |
| auto-end-turn (`AP=0 && MP=0`) lives in bridge, not engine — replay-impure | [engine_bridge.rs:894](../../../src/combat/engine_bridge.rs:894) | engine logic leaked |

---

## 2. Target state

### 2.1 Architecture

```text
combat_engine::Event           ← truth source (engine, UnitId, serializable)
       │
       │ map_engine_event(&Event, &UnitIdMap) -> Option<CombatEvent>
       ▼
CombatEvent                    ← flat enum, ECS-side (Entity)
       │
       │ format(name, content, settings) -> Option<String>
       │       └─ trait Localizer (default impl: BuiltinRu)
       ▼
console_log / log_ui           ← .filter_map(...) to drop None
```

- `CombatEvent` is **one flat enum**, not `Engine(...)` + `Local(...)`.
  Doc-comments mark each variant as either *engine mirror* or *ECS-only*.
- Mirror variants store engine enums verbatim (`TurnSkipReasonEcs`,
  `SpawnBlockedReasonEcs`, `CritFailOutcomeEcs`), not strings.
- Localization lives in `format()` via a `Localizer` trait. Default
  impl is `BuiltinRu`. No localization in the bridge.
- The bridge in PR-A stays "thick" — side-effects (Dead, ActiveCombatant,
  anim queue, next_phase) remain in translate-functions. The cleanup
  is enum-side, not pipeline-side.

### 2.2 Decisions (and their rationale)

| # | Decision | Rationale |
|---|---|---|
| D1 | `Dead`-reason on `TurnSkipped` is **suppressed** in formatter (`Option<String> = None`), not in bridge | `UnitDied` already prints "погиб" once; repeating "skips turn because dead" every round is noise. Suppressing in formatter keeps UX logic in UX layer; ECS event still carries full `reason` for tests/debug |
| D2 | Flat enum, not `Engine(...)` + `Local(...)` | enemy_popup and ~30 bridge_smoke assertions stay one-level match. Cold-reader friendly. Lost: ability to write `fn mirror_engine_event(&Event) -> EngineMirror` — judged non-critical |
| D3 | i18n via `Localizer` trait + `BuiltinRu` no-op impl, set up now | User confirmed i18n will come. +20 LOC now saves 50+ LOC and risk of missing callsite later |
| D4 | Mana-diff fix lives in engine (mini-slice in PR-A) | Currently `process_action_system` snapshots `mana_before` to diff. PR-B (side-effect extraction) cannot work cleanly without this — `MessageReader` has no "previous state". Fix: engine emits `Event::ManaRegenerated` after `PayCost` for Cast |
| D5 | `spawn_ecs_entity_from_engine_unit` and engine-trace writer **stay in `process_action_system`** | spawn is a full entity-builder, splitting it triggers id_map race; trace writer needs `Action` which `CombatEvent` does not carry |
| D6 | PR-A is atomic (one PR ~1000 LOC) | Repository never in half-state. Two-PR split with shim would require `enum CombatEvent { Legacy, New }` and double migration cost |
| D7 | PR-B (side-effect extraction in Bevy systems) deferred to post-trigger | High risk (system ordering, anim race). 80% of value comes from PR-A. Trigger could be: i18n actually landing, AI replay UI being built, or another bridge-borne bug |
| D8 | Engine schema bumps 5/6/7 deferred — no current trigger | All three are quality-of-architecture, not blockers. Group under one bump-window when a feature actually requires it |

---

## 3. PR-A — phased breakdown

**Total: ~1000 LOC + ~30 unit tests, ~7-9 hours, bumps engine SCHEMA 38→39.**

### Phase 4.0 — new flat enum + dead-variant audit (~120 LOC, 30 min)

- New module `src/game/combat_log/` (split from single file) — or keep `combat_log.rs` if size allows.
- Define new `CombatEvent` as flat enum with `#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]`.
- All variants present (both engine-mirror and ECS-only); doc-comment per variant marks origin.
- Audit dead variants — confirmed `WillOverload` is never pushed. Verify (`graphify query "WillOverload"`, ast-index `agrep`):
  - `CombatStarted` — likely used by combat_scene.rs (keep).
  - `EnergyChanged` — TBD.
  - `PoisonCleansed` — TBD.
  Remove confirmed-dead.
- Old `CombatEvent` not yet removed — coexists this phase.

### Phase 4.0b — engine mana-event mini-slice (~30 LOC, 30 min)

- In `crates/combat_engine/src/cast.rs` after `PayCost` apply, emit `Event::ManaRegenerated { unit, current, max }`.
- Bump `crates/combat_engine/src/lib.rs::SCHEMA`: 38 → 39.
- Update affected parity tests in `crates/combat_engine/tests/`.
- **Note:** name `ManaRegenerated` is awkward for spending. Acceptable for PR-A; rename to `ManaChanged` deferred to future engine cleanup.

### Phase 4.1 — engine→ECS mapper (~200 LOC + 30 tests, 2 h)

- `pub fn map_engine_event(ev: &Event, id_map: &UnitIdMap) -> Option<CombatEvent>` in `src/game/combat_log/mapper.rs`.
- Exhaustive `match` on every engine `Event` variant. Returns `None` for variants that don't surface to UI (e.g. `ActionStarted/Finished`).
- Unit tests: one per engine variant. Run via `cargo nextest run --features dev -E 'test(map_engine_event)'`.

### Phase 4.3 — atomic call-site migration (~400 LOC, 2-3 h)

Replace all `CombatEvent::OldVariant { ... }` with new flat-enum equivalents:

| File | Type of change |
|---|---|
| `src/combat/engine_bridge.rs` | translate-functions push new variants; remove mana-diff (now from engine); side-effects in place |
| `src/game/combat_log.rs` | enum definition migrated |
| `src/combat/turn_order.rs` | push sites |
| `src/combat/enemy_popup.rs` | flat match arms — minimal change |
| `src/scenario/combat_scene.rs` | push sites |
| `src/combat/advance_turn.rs` | push sites |
| `src/combat/mod.rs` | push sites |
| `src/ui/console_log.rs` | format() return type now Option<String> |
| `src/ui/log_ui.rs` | `.map(...)` → `.filter_map(...)` |
| `tests/combat/aoo.rs` | pattern matches |
| `tests/combat_engine/bridge_smoke.rs` | ~200 LOC of pattern-match diffs (heaviest test file) |
| `tests/combat_engine/legality_parity.rs` | pattern matches |
| `tests/engine_step_range_correlation.rs` | pattern matches |

### Phase 4.4 — formatter + Localizer hook (~200 LOC, 1-1.5 h)

- `impl CombatEvent { pub fn format<L: Localizer>(&self, ...) -> Option<String> }`.
- Trait `Localizer` with single impl `BuiltinRu` (no-op wrapper around current hardcoded strings).
- `log_ui.rs:44` callsite updated:
  ```rust
  t.0 = log.0.iter()
      .filter_map(|e| e.format(name, &content, settings.crit_fail_die)
          .map(|s| format!("{s}\n")))
      .collect();
  ```
- `console_log.rs:20` callsite uses `if let Some(line) = ... { println!("{line}"); }`.

### Phase 4.5 — legacy cleanup (+50/-150 LOC, 1.5 h)

- **`TurnSkipped`**: add `reason: TurnSkipReasonEcs` field. Formatter:
  - `TurnSkipReasonEcs::Stunned` → `Some(format!("  ○ {} пропускает ход [оглушён]", ...))`
  - `TurnSkipReasonEcs::Dead` → `None` (suppress)
- **`PoisonTick`/`PoisonCleansed`** → renamed to mirror engine: `DotTicked`/`StatusRemoved` (or unified into `StatusRemoved` if cleanse semantics overlap).
- **`SummonBlocked.reason: String`** → enum `SpawnBlockedReasonEcs` (mirror engine `SpawnBlockedReason`). Localization in formatter.
- **`CritFailSideEffect.effect_name: String`** → enum `CritFailOutcomeEcs` (mirror engine `CritFailOutcome`). For `ApplyStatus(id)` variant — formatter resolves `id` through `ContentView` (proper localization, unlike current).
- **`HealResult.formula`** → field removed. Formatter prints `"    лечение: +{N} HP ({name})"` without formula.

---

## 4. Risks and mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| SCHEMA bump 38→39 breaks old `engine.jsonl` traces | Medium | Update fixture traces; run `replay_event_divergence_detected` parity tests; old user-generated traces simply won't replay (acceptable — internal tool) |
| `id_map` race when `UnitSpawned` and `UnitMoved` for new uid land in same step | Low (in PR-A) | `spawn_ecs_entity_from_engine_unit` stays in `process_action_system` per D5 — id_map insertion stays atomic with event-stream order |
| `bridge_smoke.rs` migration is mechanical-but-tedious (~30 assertions) | Low | Chore work, no semantic change. Allocate explicit budget |
| Mana-diff removal from bridge changes behavior if engine `ManaRegenerated` emission has wrong timing | Medium | Cover with parity test in 4.0b before migrating 4.3 |
| Formatter `Option<String>` change breaks a third callsite I missed | Low | `cargo check --features dev` will catch — exhaustive type check |

---

## 5. Critical files

| File | Role | Effort |
|---|---|---|
| [src/game/combat_log.rs](../../../src/game/combat_log.rs) (310 LOC) | enum + format definition | full rewrite |
| [src/combat/engine_bridge.rs](../../../src/combat/engine_bridge.rs) (~1500 LOC) | translate-functions (lines 639, 990, 1067, 1270), `apply_phase_ecs_writes` (408), `spawn_ecs_entity_from_engine_unit` (465) | enum migration only; side-effects unchanged |
| [crates/combat_engine/src/event.rs](../../../crates/combat_engine/src/event.rs) | truth source for mirror | bump SCHEMA |
| [crates/combat_engine/src/cast.rs](../../../crates/combat_engine/src/cast.rs) | emit `ManaRegenerated` after PayCost | +~20 LOC |
| [src/combat/enemy_popup.rs](../../../src/combat/enemy_popup.rs) | UI consumer matching specific variants | minimal — flat enum saves us |
| [tests/combat_engine/bridge_smoke.rs](../../../tests/combat_engine/bridge_smoke.rs) | ~30 pattern-match assertions | heaviest test migration |

---

## 6. Deferred work (PR-B + engine schema bumps)

### PR-B — side-effect extraction (~400 LOC, 4-6 h)

After PR-A lands. Moves these out of translate-functions into separate Bevy systems reading `CombatEvent` (or a dedicated `EngineEventMsg`) via `MessageReader`:

- `commands.entity(ent).insert(Dead)`
- `commands.entity(ent).remove::<ActiveCombatant>()`
- `commands.entity(ent).insert(ActiveCombatant)` (mid-round)
- `next_phase.set(CombatPhase::StartRound)`
- `anim_queue.0.push_back(PendingAnim::Movement)`

**Stays in `process_action_system`** (per D5/D7):
- `spawn_ecs_entity_from_engine_unit` — id_map race risk
- engine-trace writer — needs `Action`, not `Event`
- auto-end-turn `step(EndTurn)` cascade — engine API change, see Stage 6

Uses existing `PendingPhaseTransitions` + `apply_phase_transitions_system` (engine_bridge.rs:408, 484) as a pattern.

**Trigger**: i18n actually shipping, AI replay UI, or another bridge-borne bug.

### Stages 5/6/7 — engine API enrichment

Each requires engine SCHEMA bump and parity-test churn. Group under one bump-window if multiple come at once.

| Stage | What | Trigger | Approx size |
|---|---|---|---|
| 5 | `Event::DotDamaged` atomic event (replaces `StatusTicked + UnitDamaged` pair) | When 3rd DoT variant or new effect breaks `pending_status_tick` state-machine in bridge | engine +50 / bridge -50, SCHEMA 39→40 |
| 6 | auto-end-turn in engine (`Action::Cast` checks AP=0 && MP=0 → auto-emit `AdvanceTurn`) | When AI agenda-builder grows logic depending on auto-end behavior | engine +50 / bridge -30, SCHEMA 40→41, **high risk** (AI surface) |
| 7 | `Event::EnergySpent` parity to `EnergyRegenerated` | When energy is spent outside `Action::Cast` (toggle auras, drain abilities) | engine +20 / bridge -40, SCHEMA 41→42 |

---

## 7. Open questions

- **Naming**: keep `CombatEvent` or rename to `CombatLogEntry`? `LogEntry` is more accurate (UI-side log entry, not "everything that happens in combat" — that's engine `Event`). Decision deferred to start of PR-A.
- **Module layout**: single `combat_log.rs` (current) or split into `combat_log/{mod.rs, enum.rs, mapper.rs, format.rs, localizer.rs}`? Lean toward split if 4.0 brings the file over ~400 LOC.
