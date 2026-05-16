# Phase 4 — Turn queue, round flow, EndTurn first-class

**Parent plan:** `docs/ai/rework/unisim.md` §5.4
**Predecessor:** `docs/ai/rework/step_unisim3_5_plan.md` (Phase 3.5 — tagged `unisim/phase3.5-complete`)
**Goal:** Engine owns the round/turn state machine **and** the two remaining ECS-side combat logic systems (auras + phase transitions). `TurnQueue` lifts into `CombatState`; `RoundPhase` becomes engine-authoritative. `Action::EndTurn` grows from a no-op marker (Phase 3) into a real action that advances the queue, fires EndRound/StartRound transitions, and skips dead/stunned actors (status OR aura). Auras become pure-presence engine queries (no stored state, no projection bridge). Boss phase transitions become reactive `Effect::EnterPhase` derived from `Effect::Damage`, with revival preempting death by construction. After Phase 4, **no combat logic remains in Bevy systems** — only orchestration, projection, and UI translation.
**Timebox:** ~1.5–2 weeks (extended from original §5.4 estimate of 1 week to absorb the aura + phase migrations that close the D4/D5 collision and the D8 1-frame lag).

**Out of scope (deferred to later phases):**
- Replay first-class (Phase 5). Phase 4 only ensures `Action::EndTurn` is a first-class event in the stream — does NOT change log schema.
- **Sticky/duration aura effects** (status persists N rounds after exiting aura radius). Phase 4 commits to **pure-presence semantics**: aura effect active iff `distance(source, target) ≤ radius`. Sticky-effects, when needed in future content, will be expressed via "on-enter cast" path (entering aura radius casts a regular ability that applies a duration status) — see D5.
- Bug 2 (HexPositions panic on respawn over corpse). Orthogonal; stays separate.
- `SnapshotContentView::unit_template` real impl. AI beam search still does not enumerate Summon candidates; the None stub remains. Wiring deferred to Phase 6 unless AI scoring is expanded.

---

## 1. Scope

**IN:**

**Engine extensions:**
- `CombatState.turn_queue: TurnQueue` field. Mirrors the current ECS `Res<TurnQueue>` shape (`order: Vec<UnitId>`, `index: usize`). Methods: `current()`, `advance()`, `is_empty()`, `wrapped_after(prev_idx)`.
- `CombatState.phase: RoundPhase` — already exists; Phase 4 makes it transition-driven (writes happen inside `step(Action::EndTurn)`, not externally).
- New atomic `Effect::AdvanceTurn` — pops the queue cursor; if it wraps, derives `Effect::BumpRound`. If next slot is a dead/stunned actor and the engine knows skip semantics, derives `Effect::AdvanceTurn` recursively (depth bounded by queue length — see gotchas).
- New atomic `Effect::BumpRound` — increments `state.round`, sets `phase = PreRound`, fires `RefreshAggregates` on every alive unit (for status duration consistency at round boundary), then transitions `phase = ActorTurn`. Emits `Event::RoundStarted`.
- New `Event::TurnEnded { actor }`, `Event::TurnStarted { actor }`, `Event::RoundStarted { round }`. (Phase 3 retro listed `TurnEnded` as already wired via `CombatEvent::TurnEnded` log; the engine variant is new — Phase 3's `Action::EndTurn` only emitted `ActionStarted`/`ActionFinished`.)
- `step(Action::EndTurn)` arm grows from minimal marker to: (1) call `state.tick_actor_statuses(actor, content)` if not already called this turn — sirota DoT remains correct, (2) push `Effect::AdvanceTurn`, (3) emit `Event::TurnEnded { actor }` before `ActionFinished`. After the queue effect resolves, emit `Event::TurnStarted { next_actor }`. If the queue wrapped, the `BumpRound` derived effect fires first, then `TurnStarted` for the next slot's actor.
- New engine helper `CombatState::start_round(content)` — call after `BumpRound`. Resets `reactions_left` on alive units, NOT initiative (initiative is per-combat, see D2).
- `CombatState` constructor extension: takes initial `Vec<UnitId>` for the queue and an `initial_index`. Bridge passes the rolled+sorted order from `build_turn_order`.

**Bridge:**
- New Bevy message `ActionInput::EndTurn { actor }` variant (today `EndTurn` is a standalone `Message`). Player + AI writers continue to use the legacy `EndTurn` message during the transition; bridge consolidation lands in step 4e.
- `process_action_system` routes `ActionInput::EndTurn` → `step(Action::EndTurn)`.
- New translator arms in `translate_action_events` (or extending `translate_tick_events`): `Event::TurnEnded` → `CombatEvent::TurnEnded`, `Event::TurnStarted` → `CombatEvent::TurnStarted` + insert `ActiveCombatant`, `Event::RoundStarted` → `CombatEvent::RoundStarted` + remove `ActiveCombatant` from all + set `NextState<CombatPhase>` to `StartRound`.
- `init_state_from_ecs` extended: also reads `TurnQueue.order`, maps via `id_map`, sets `state.turn_queue` + `state.phase = ActorTurn`. **The ECS `Res<TurnQueue>` is kept** for the projection layer (UI reads it for turn-order panel); `project_state_to_ecs` writes the engine queue back into the Res. See D1 — two-step decision.
- `engine_turn_start_system` (existing) — change: replace its `last_active` change-detect with reading `Event::TurnStarted` from the engine stream. Eliminates the Local<Option<Entity>> latch and the race where engine hands off mid-frame.
- **Aura migration to pure-presence query** (replaces the projection-bridge idea — see D5). Engine gains `ContentView::auras_of(source_id) -> Vec<AuraDef>` and helper `state.aura_effects_on(target, content) -> AuraEffects` that walks all alive aura sources and folds bonuses for those within radius. `refresh_aggregates` consults `aura_effects_on` to compute `unit.speed`/`unit.armor_bonus` — same path as status-derived bonuses, just an additive fold over a second source. Skip-stunned predicate inside `Effect::AdvanceTurn` queries `aura_effects_on(next_actor).skips_turn` in addition to `unit.statuses`. **Aura state is never stored** in `unit.statuses` — derived purely from `(positions, content.auras_of)` at query time. Closes the MP regression (Phase 3.5b) by construction and eliminates the D4/D5 collision without a projection layer.
- **Aura combat-log events via diff-on-move.** `step()` snapshots `aura_membership_set(state)` before `Effect::MovePosition` (for actor AND any source whose move re-scans neighbors) and computes the after-set. Delta emits `Event::AuraStatusGained { target, source, status_id }` and `Event::AuraStatusLost`. Bridge translator writes `CombatEvent::StatusApplied`/`StatusRemoved` for the log. Diff cost is O(aura_sources × |adj(target)|) per move — trivial for 10-unit combats.
- **Phase transitions to engine — canonical reactive model.** New `Effect::EnterPhase { unit, phase_idx }` derived from `Effect::Damage` when `ContentView::check_phase_trigger(unit, new_hp, max_hp)` returns `Some`. **Phase trigger preempts death**: in `apply_effect(Damage)`, after applying the HP delta, check phase trigger BEFORE deriving `Effect::Death`. If phase fires, derived effects cascade as atomic mutations (`Effect::SetMaxHp`, `Effect::Heal { full: true }`, `Effect::SetBaseSpeed`, `Effect::SetArmor`, `Effect::RefreshAggregates`) — boss never enters `Dead` state when phase revives.
- New engine `Event::PhaseEntered { unit, phase_idx, prev_max_hp, new_max_hp }` — observable fact for bridge/UI consumers.
- `ContentView::check_phase_trigger(unit_id, new_hp, max_hp) -> Option<PhaseTransition>` — bridge abstracts `EnemyPhases.pending` data layout. `PhaseTransition` carries engine-side resolved deltas (max_hp, armor, base_speed, heal_to_full). ECS-only deltas (name, abilities, AxisProfile, flavor) live in `EnemyPhases.pending[phase_idx]` and are read by the bridge translator on `Event::PhaseEntered`.

**Deletions:**
- `src/combat/turn_order.rs::build_turn_order` shrinks: initiative rolling stays ECS-side (D2), but queue construction and the `NextState<CombatPhase>::set(AwaitCommand)` transition is replaced by a call to `combat_state.0.start_round(...)` followed by the projection writing back to ECS.
- `src/combat/advance_turn.rs::advance_turn_system` collapses: most of the dead-skip loop + wrap-detection moves into the engine (`Effect::AdvanceTurn` recursion). The Bevy system becomes a thin shim that just writes `ActionInput::EndTurn` (already in queue via player/AI) and observes the engine's `Event::RoundStarted` to drive `NextState<CombatPhase>::set(StartRound)`. Wrap-detection unit tests in `advance_turn.rs::tests` migrate to `crates/combat_engine/tests/turn_queue.rs`.
- `src/combat/skip_dead.rs::skip_dead_turn_system` + `skip_stunned_turn_system`: both DELETED. Engine handles skip-on-AdvanceTurn (dead: predicate `!unit.is_alive()`; stunned: predicate combines `unit.statuses` walk **and** `aura_effects_on(actor, content).skips_turn`). The TurnSkipped log event is emitted by a new `Event::TurnSkipped { actor }` arm.
- `src/combat/auras.rs::apply_auras_system`: DELETED. Replaced by engine query-time evaluation; no Bevy system needed.
- `src/combat/phases.rs::phase_transition_system`: DELETED. Replaced by `Effect::EnterPhase` reactive cascade in engine + bridge translator for ECS-only deltas.

**Pipeline change:**
- StartRound chain: `build_turn_order` becomes `bridge_start_round` (calls engine `start_round` then projects). Initiative roll stays first-round-only inside the bridge (uses `DiceRngRes`).
- TurnStart chain: shrinks to just `engine_turn_start_system`. `skip_dead_turn_system`, `skip_stunned_turn_system`, `apply_auras_system` all removed.
- Execute chain: `phase_transition_system` removed. Engine handles via `Effect::EnterPhase` cascade; bridge translator picks up `Event::PhaseEntered` and writes ECS-only deltas (name, abilities, AxisProfile, Dead removal if revival).
- Finalize chain: `advance_turn_system` becomes minimal — just observe engine `RoundStarted` to flip `CombatPhase`.

**Param-count consolidation:**
- `process_action_system` (17 params after Phase 3.5) gains `EndTurn` routing + `MessageWriter<CombatEvent>` for turn events but should not add new resources. Phase 4 bundles `tag_cache, mats, token_mesh` into a `SystemParam` newtype `VisualAssets` (see D6) — opportunistic cleanup. Drops param count to ~14.

**AI integration:**
- `SimState::apply_endturn(actor)` (already defined in Phase 3, unwired) now becomes a real beam-search hook: at the end of each plan branch, `apply_endturn` advances ticks + queue inside the cloned sim state, so scoring sees post-handoff state. (Phase 3 retro deferred wiring "until Phase 4 turn-queue context exists" — Phase 4 provides it.)

**PresetInitiative:**
- Stays ECS-only (D7). It is a *combat-bootstrap* mechanism (carry initiative across in-mission restart), not a steady-state engine concept. `build_turn_order` consults it on round 1; engine sees the final order via `init_state_from_ecs`.

**Preserved (ECS-side):**
- `enemy_popup::queue_enemy_popup` (UI).
- `PresetInitiative`, `check_victory_system`.
- ECS `EnemyPhases` component — bridge reads `pending[phase_idx]` for ECS-only deltas on `Event::PhaseEntered`; engine sees only the resolved `PhaseTransition` via `ContentView::check_phase_trigger`.

**OUT (deferred):**
- Sticky/duration aura effects (model is pure-presence; D5). Future content uses on-enter-cast pattern if needed.
- `SnapshotContentView::unit_template` real impl. Phase 6 unless AI needs summon plans.
- HexPositions respawn-over-corpse panic. Orthogonal.

---

## 2. Architecture diff vs Phase 3.5

```
Phase 3.5:                                  Phase 4 target:

  TurnQueue in Res<TurnQueue>;                TurnQueue in CombatState.turn_queue;
  build_turn_order writes Res,                Res<TurnQueue> kept as one-way
  advance_turn_system mutates Res             projection from engine (UI/panel read).

  Action::EndTurn = no-op marker              Action::EndTurn = real:
   (ActionStarted/Finished only).             pushes Effect::AdvanceTurn, fires
                                              Event::TurnEnded → TurnStarted
                                              (or RoundStarted then TurnStarted).

  skip_dead + skip_stunned in ECS             Engine skip handled inside
   write EndTurn messages.                    Effect::AdvanceTurn recursion;
                                              Event::TurnSkipped emitted.

  advance_turn_system:                        advance_turn_system: ~20 lines —
   ~80 lines of skip+wrap+sirota              observe Event::RoundStarted →
   logic, calls tick_actor_statuses           NextState<CombatPhase>::set.
   directly for dead skip.

  Aura speed_bonus written to ECS             apply_auras_system DELETED.
   StatusEffects after Phase 3.5 MP           Engine computes aura effects
   refill ran → MP refilled with              via aura_effects_on(target, content)
   STALE base_speed (regression).             query — pure-presence,
                                              source/target both alive + in radius.
                                              No state, no projection.

  phase_transition_system (ECS) in            phase_transition_system DELETED.
   Execute, runs after damage but             Effect::EnterPhase derived from
   before advance. Same-frame revive          Effect::Damage cascade BEFORE
   via remove::<Dead>. Multi-threshold        Effect::Death is derived. Revival
   AoE capped to one phase per frame.         preempts death by construction.
                                              Each AoE hit fires its own phase
                                              check; multi-threshold cascades
                                              naturally.

  RoundPhase exists in state but              RoundPhase transitions are
   never advanced by engine.                  driven inside step(EndTurn)
                                              effect cascade.

  No Event::TurnEnded/TurnStarted/            New: Event::TurnEnded,
   RoundStarted in engine stream.             TurnStarted, TurnSkipped,
                                              RoundStarted. Bridge translates
                                              into existing CombatEvent variants.

  process_action_system: 17 params,           process_action_system: ~14 params
   visual asset Res inlined.                  after VisualAssets newtype bundle.
```

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/src/state.rs` | Add `turn_queue: TurnQueue` field; add `start_round(content)`, `aura_effects_on(target, content)`, queue accessors |
| `crates/combat_engine/src/state.rs` (new submod `turn_queue`) | `TurnQueue { order: Vec<UnitId>, index: usize }`, `advance()`, `current()`, `is_empty()`, `wrapped_after(prev)` |
| `crates/combat_engine/src/effect.rs` | Add `Effect::AdvanceTurn`, `Effect::BumpRound`, `Effect::EnterPhase`, `Effect::SetMaxHp`, `Effect::SetBaseSpeed`, `Effect::SetArmor`; `apply_effect` arms with skip-dead/skip-stunned predicate, wrap derivation, phase preempts death |
| `crates/combat_engine/src/event.rs` | Add `Event::TurnEnded`, `TurnStarted`, `TurnSkipped`, `RoundStarted`, `PhaseEntered`, `AuraStatusGained`, `AuraStatusLost`; `effect_to_event` arms |
| `crates/combat_engine/src/step.rs` | Grow `Action::EndTurn` arm: tick statuses, push `Effect::AdvanceTurn`, emit turn/round events from cascade. Wrap `MovePosition`-emitting actions with aura-membership diff (before/after snapshots → `AuraStatusGained`/`Lost` events) |
| `crates/combat_engine/src/content.rs` | Extend `ContentView` trait: `auras_of(unit_id) -> Vec<AuraDef>`, `check_phase_trigger(unit_id, new_hp, max_hp) -> Option<PhaseTransition>`. New types: `AuraDef { radius, status_id, applies_to_team_relation }`, `PhaseTransition { max_hp, armor, base_speed, heal_to_full }` |
| `crates/combat_engine/src/action.rs` | No change to `Action::EndTurn` enum variant; semantics change only in `step` arm |
| `src/combat/engine_bridge.rs` | Extend `init_state_from_ecs` to populate `turn_queue` and `phase`; new `bridge_start_round` system that calls `state.start_round`; new translator arms for turn/round events, `Event::PhaseEntered` (writes ECS Name/Abilities/AxisProfile/removes Dead), `Event::AuraStatusGained`/`Lost` (CombatLog only); extend `EcsContentView` with `auras_of` (reads ECS `Aura` component) and `check_phase_trigger` (reads `EnemyPhases.pending`); `process_action_system` routes `ActionInput::EndTurn`; new `VisualAssets` SystemParam newtype bundling `tag_cache`/`mats`/`token_mesh` |
| `src/combat/turn_order.rs` | `build_turn_order` becomes `bridge_start_round`: keep first-round initiative roll + PresetInitiative consumption, delegate queue build + phase transition to engine via `state.start_round` |
| `src/combat/advance_turn.rs` | Collapse `advance_turn_system` to a ~20-line shim that observes engine events; remove `tick_actor_statuses` direct call (engine handles via `Effect::AdvanceTurn` recursion); migrate wrap-detection unit tests to engine |
| `src/combat/skip_dead.rs` | **DELETED** |
| `src/combat/auras.rs` | **DELETED** |
| `src/combat/phases.rs` | **DELETED** |
| `src/combat/pipeline.rs` | Drop `skip_dead`/`skip_stunned`/`apply_auras_system` from TurnStart chain; drop `phase_transition_system` from Execute chain; rename `build_turn_order` → `bridge_start_round`; `process_action_system` routing for new `ActionInput::EndTurn` variant |
| `src/game/messages.rs` | Add `EndTurn { actor }` variant to `ActionInput` enum; keep legacy `EndTurn` standalone Message as a transitional alias (deletes in 4e) |
| `src/combat/mod.rs` | Remove `pub mod skip_dead;`, `pub mod auras;`, `pub mod phases;` |
| `src/combat/ai/plan/sim.rs` | Wire `apply_endturn` into beam-search expansion (end-of-branch) |
| `tests/combat_engine/turn_queue.rs` (new) | Engine tests: advance, wrap, dead-skip, stunned-skip (via aura + via direct status), full cycle, round bump fires RefreshAggregates |
| `tests/combat_engine/end_turn.rs` (new) | Engine tests: `step(EndTurn)` event order; sirota-DoT still ticks via EndTurn path |
| `tests/combat_engine/aura.rs` (new) | Engine tests: aura-effect query (in-range / out-of-range / dead source / cross-team filter), aura-stun-on-next-actor skip, diff-on-move events |
| `tests/combat_engine/phase.rs` (new) | Engine tests: phase trigger fires on Damage crossing threshold, phase preempts death (revival, no Dead transient), multi-threshold AoE hit fires multiple phases |
| `tests/combat.rs` | Adapt round-flow integration tests; remove tests for `skip_dead_turn_system`/`skip_stunned_turn_system`/`apply_auras_system`/`phase_transition_system` |
| `docs/ai/rework/step_unisim4_plan.md` | This file |
| `docs/ai/rework/unisim.md` | Append Phase 4 retrospective in §5.4 after gate |

---

## 4. Sub-step decomposition

Each sub-step lands independently with `cargo check --all-targets` clean and `cargo test` green.

| Step | Title | What lands |
|---|---|---|
| **4a** | Engine TurnQueue + start_round | `CombatState.turn_queue` field, `TurnQueue` type, `start_round(content)` method (resets reactions_left, sets phase). `init_state_from_ecs` reads `Res<TurnQueue>` and populates the engine queue. Engine unit tests for queue advance / wrap / current. No `Effect::AdvanceTurn` yet — pure data + projection. Bevy still owns advance logic. |
| **4b** | Engine AdvanceTurn + BumpRound effects + step(EndTurn) growth | `Effect::AdvanceTurn` + `Effect::BumpRound` with apply_effect arms (skip-dead + skip-stunned-via-statuses predicates, wrap derivation, recursion bound). `Event::TurnEnded`, `Event::TurnStarted`, `Event::TurnSkipped`, `Event::RoundStarted`. `step(Action::EndTurn)` grows to push `Effect::AdvanceTurn` + emit turn events. Engine unit tests for: mid-round handoff, end-of-round wrap, dead-skip, stunned-skip (status-applied), all-dead-wrap, sirota tick inside skip. Aura-stun skip deferred to 4c. |
| **4c** | Aura pure-presence query + skip predicate extension | `ContentView::auras_of(source)` and `state.aura_effects_on(target, content)` query. `refresh_aggregates` folds aura bonuses into `unit.speed`/`unit.armor_bonus`. Skip-stunned predicate in `Effect::AdvanceTurn` extended to OR with `aura_effects_on(actor).skips_turn`. Diff-on-move: `step()` wraps `Effect::MovePosition` with aura-membership snapshots and emits `Event::AuraStatusGained`/`AuraStatusLost`. Engine tests for aura skip and membership diff. `EcsContentView::auras_of` reads ECS `Aura` component. Still wired alongside the legacy `apply_auras_system` — both run; correctness verified by parity. |
| **4d** | Effect::EnterPhase reactive trigger | `Effect::EnterPhase { unit, phase_idx }` + cascade into `Effect::SetMaxHp`, `Effect::SetBaseSpeed`, `Effect::SetArmor`, `Effect::Heal { full }`, `Effect::RefreshAggregates`. `apply_effect(Damage)` checks `content.check_phase_trigger` AFTER hp delta but BEFORE deriving `Effect::Death` — phase preempts death. `Event::PhaseEntered { unit, phase_idx, ... }`. `ContentView::check_phase_trigger` + `EcsContentView` impl reading `EnemyPhases.pending`. Bridge translator for `PhaseEntered`: writes ECS Name, Abilities, AxisProfile, removes `Dead` if applicable, writes `CombatEvent::PhaseEntered`. Both engine path and legacy `phase_transition_system` run; correctness verified by parity tests; legacy system runs as no-op once engine fires (idempotency check). Engine tests for preempt-death and multi-threshold AoE. |
| **4e** | Bridge wiring + ECS deletion sweep | `ActionInput::EndTurn` variant; `process_action_system` routes it; bridge translators wire turn/round events to `CombatEvent` + insert/remove `ActiveCombatant` + set `NextState<CombatPhase>::StartRound`. Player + AI EndTurn writers migrated; legacy `EndTurn` standalone message DELETED. DELETE: `src/combat/skip_dead.rs`, `src/combat/auras.rs`, `src/combat/phases.rs`. `pub mod` lines removed from `src/combat/mod.rs`. Pipeline registrations removed. `advance_turn_system` collapsed to event-observer shim. Wrap-detection unit tests migrated from `advance_turn.rs` to engine. Full-suite + manual playtest gate. |
| **4f** | VisualAssets newtype + sim wiring + retrospective | `VisualAssets` SystemParam newtype bundling `tag_cache`/`mats`/`token_mesh`/`grid_offset`; `process_action_system` and `spawn_ecs_entity_from_engine_unit` updated. Wire `SimState::apply_endturn` at end of beam-search branch in `sim.rs`. AI mining regression check: agenda mix delta logged. Retrospective drafted; user applies tag `unisim/phase4-complete`. |

---

## 5. Decisions (locked)

### D1. TurnQueue location — lift to engine; keep ECS Res as one-way projection
`CombatState.turn_queue` becomes the source of truth. The existing `Res<TurnQueue>` is **retained as a projected mirror** because UI consumers (turn-order panel, ability overlay) read it as a Bevy `Res`, and rewriting them now would balloon Phase 4. `project_state_to_ecs` writes engine queue → Res after every `process_action_system` call. UI is read-only; no UI system mutates `Res<TurnQueue>` after Phase 4.

**Rationale:** Two-step migration avoids a UI-wide refactor in Phase 4. The projection is one-directional (engine → ECS), matching the Phase 1+ pattern for HP/AP/etc. Phase 6 cleanup can delete the Res if UI is migrated.

### D2. Initiative rolling — stays in bridge, not engine
Initiative is *per-combat-start* state (rolled once on round 1, preset-restoreable across in-mission restart). The engine has `DiceSource` but no concept of "combat start" — combats begin via `OnEnter(AwaitCommand)`. Engine's `start_round` accepts a pre-built `Vec<UnitId>`; bridge does the d20+dex_mod roll and ordering on round 1, then passes the final order to engine. Subsequent rounds reuse the engine's stored queue (just `index = 0`).

**Rationale:** Initiative tracking is tied to ECS `Initiative` component (savegame, UI, dev tools). Keeping the roll in bridge avoids spreading combat-bootstrap logic into the engine. Engine handles per-round queue advance (the hot path); bridge handles per-combat queue construction (the cold path).

### D3. `Action::EndTurn` is the only path to advance the queue
No "implicit advance" anywhere. Player presses End Turn → `ActionInput::EndTurn` → `step(EndTurn)`. AI emits `ActionInput::EndTurn` after its plan finishes. Dead actors emit `ActionInput::EndTurn` via the engine's internal skip recursion (one `Effect::AdvanceTurn` per skip). Stunned actors same.

**Rationale:** First-class `EndTurn` action is exactly what Phase 5 (replay) needs — the log is a sequence of `Action`s. Implicit advances would make replay reconstruction lossy. Also matches §6.3 of `unisim.md`: per-turn resolution is self-contained.

### D4. Skip-dead / skip-stunned — predicate inside `Effect::AdvanceTurn`
Both ECS systems (`skip_dead_turn_system`, `skip_stunned_turn_system`) deleted. `Effect::AdvanceTurn` arm checks `state.unit(next).is_alive()` and `state.unit(next).statuses.iter().any(|s| content.status_def(&s.id).is_some_and(|d| d.skips_turn))`. If skipped: emit `Event::TurnSkipped { actor, reason }`, push another `Effect::AdvanceTurn` (recursion bounded by queue length).

**Rationale:** Consolidates skip logic in one place; eliminates two Bevy systems and the inter-system race ("did skip_dead run before AI got a turn?"). Predicate has access to `ContentView` via the standard `apply_effect` signature. Stunned actors still get `TurnSkipped` log entry; AP/MP drain (previously done by `skip_stunned_turn_system`) is now unnecessary because `start_actor_turn` is never called for skipped actors.

### D5. Auras — pure-presence query, no stored aura state
Aura effects (speed/armor bonus, skips_turn, causes_disadvantage, damage_taken_bonus) are **not stored** in `unit.statuses`. They're computed at query time by `state.aura_effects_on(target, content)`: walk alive aura sources, filter by `distance(source.pos, target.pos) ≤ radius`, fold bonuses. `refresh_aggregates` consults this query when computing `unit.speed`/`armor_bonus`. Skip-stunned predicate ORs status walk with `aura_effects_on(actor).skips_turn`.

**Combat-log integrity** preserved via diff-on-move: `step()` snapshots `aura_membership_set` before any `Effect::MovePosition` and emits `Event::AuraStatusGained`/`AuraStatusLost` for the delta. Bridge translates to `CombatEvent::StatusApplied`/`StatusRemoved`.

**Sticky/duration aura effects are explicitly OUT** (Phase 4 §0). Future content needing them uses the on-enter-cast pattern: an aura definition that casts a regular ability when a unit enters its radius, applying a normal duration-bearing status. This keeps the engine model clean (auras = presence-gated; durations = action-applied statuses).

**Rationale:** Event-driven Enter/Exit was considered and rejected. It needs a `via_aura` flag on `ActiveStatus`, dual write-paths to the same status list, cascade effects on source-move/source-death/spawn, and warm-scan logic at combat start — all to gain the ability to express sticky effects that the on-enter-cast pattern handles cleanly. Pure-presence wins on simplicity, eliminates D4 ordering coupling by construction, makes clones cheaper in sim, and removes `apply_auras_system` entirely. The diff-on-move snapshot is O(aura_sources × adj_radius) — trivial for typical combat scale.

**Engine method shape:**
```rust
pub struct AuraEffects {
    pub speed_bonus: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    pub skips_turn: bool,
    pub causes_disadvantage: bool,
}
impl CombatState {
    pub fn aura_effects_on(&self, target: UnitId, content: &dyn ContentView) -> AuraEffects;
    pub fn aura_membership_set(&self, content: &dyn ContentView) -> HashSet<(UnitId, UnitId, StatusId)>; // (target, source, status_id)
}
```

### D6. `process_action_system` param-count — bundle in Phase 4
17 params (Phase 3.5 retro) → ~14 after `VisualAssets` SystemParam newtype. This is opportunistic cleanup that fits naturally with Phase 4's bridge changes (we're already touching the system to add EndTurn routing). Bundling `tag_cache: Res<AbilityTagCache>`, `mats: Res<HexMaterials>`, `token_mesh: Res<TokenMesh>`, `grid_offset: Res<HexGridOffset>` into `VisualAssets` keeps the spawn-entity helper signature clean too.

**Rationale:** Cheap, addresses Phase 3.5 carry-over, no behavior change. Risk: Bevy SystemParam derive macro surface — already used elsewhere in the codebase, no new dependency.

### D7. `PresetInitiative` — stays ECS Resource
`PresetInitiative` is consumed *once* on round 1 of a restart, then cleared. It's a bootstrap-only piece of state, tied to scenario save/restart. Moving it into engine would require engine-side awareness of "first round of combat" which is a Bevy state-machine concept (`OnEnter(AwaitCommand)`).

**Rationale:** Engine doesn't need a notion of combat lifecycle beyond rounds. Bridge consumes `PresetInitiative` inside `bridge_start_round` (renamed `build_turn_order`) and passes the final ordered `Vec<UnitId>` to `state.start_round(...)`. Engine is none the wiser. Aligns with D2.

### D8. Phase transitions — `Effect::EnterPhase` reactive on Damage, preempts Death
Engine becomes authoritative for HP-threshold phase changes. `apply_effect(Damage)` checks `ContentView::check_phase_trigger(target, new_hp, max_hp)`:
- If `Some(transition)` — push `Effect::EnterPhase { unit, phase_idx }` whose cascade applies engine-side mutations (`SetMaxHp`, `SetBaseSpeed`, `SetArmor`, `Heal { full: heal_to_full }`, `RefreshAggregates`).
- Else if `target.hp ≤ 0` — push `Effect::Death`.

This is exactly the same reactive pattern as Damage→Death and Damage→GainRage; phase becomes a third derivative of Damage. **Revival semantics are clean:** if a phase fires when hp would otherwise be ≤ 0, `Heal { full }` sets `hp = new_max_hp > 0` BEFORE the Death check sees the unit — the boss never enters `Dead` state. No `commands.entity(e).remove::<Dead>()` dance.

Engine emits `Event::PhaseEntered { unit, phase_idx, prev_max_hp, new_max_hp }`. Bridge translator:
- Reads `EnemyPhases.pending[phase_idx]` for ECS-only deltas: `Name`, `Abilities`, optional `CombatStats` (full, not just the engine subset), `AxisProfile` re-infer, `flavor` text.
- Writes those to the ECS entity.
- Pops `pending[0]` from `EnemyPhases`.
- Writes `CombatEvent::PhaseEntered { actor, prev_name, next_name, flavor }`.

**Per-frame phase cap removed.** Today's `phase_transition_system` caps at one phase per frame to avoid cascades on huge hits. With engine model, each `Effect::Damage` in the effect queue gets its own phase check — multi-threshold AoE naturally fires multiple phases.

**ContentView surface:**
```rust
pub struct PhaseTransition {
    pub new_max_hp: i32,
    pub new_armor: i32,
    pub new_base_speed: i32,
    pub heal_to_full: bool,
}
pub trait ContentView {
    fn check_phase_trigger(&self, unit_id: UnitId, new_hp: i32, max_hp: i32) -> Option<(usize, PhaseTransition)>; // (phase_idx, deltas)
}
```

**Rationale:** D8 reversed from "stays ECS" to "migrate" once it became clear (a) it costs ~2 days, not a week (just a `check_phase_trigger` + reactive derivation, no Unit-field changes — `max_hp`, `armor`, `base_speed` already exist on engine `Unit`); (b) it removes the only remaining ECS-side combat-logic system after auras/skip migrate; (c) sim now sees boss revival in its forecast, which fixes a long-standing scoring gap; (d) revival-preempts-death is architecturally cleaner than insert-Dead-then-remove-Dead.

---

## 6. Sub-step kickoff order

Strict order: 4a → 4b → 4c → 4d → 4e → 4f.

Each sub-step:
1. `cargo check --all-targets` green.
2. Sub-step's targeted tests green.
3. Full suite (`cargo test`) green.
4. Commit with `ai/unisim Phase 4 step Nx: <title>` (mirror Phase 3.5 commit style).
5. User review before next sub-step.

**Rationale for ordering:**
- **4a** is a pure additive engine change (data + projection in). Cannot regress anything; the engine just gains a queue field that nothing reads yet.
- **4b** adds the effect machinery AND wires `step(EndTurn)` to use it. Engine becomes capable of running a full round cycle internally; bridge translation still missing, so the legacy `advance_turn_system` is the live path. Skip-stunned only via direct status; aura-stun coverage deferred to 4c. Engine-only unit tests.
- **4c** adds aura query + diff-on-move. Legacy `apply_auras_system` keeps running in parallel — engine query is verified by parity assertions (cross-check engine aura result vs ECS `StatusEffects` for aura-applied entries). Skip-stunned predicate now sees aura stun. Bridge `EcsContentView::auras_of` reads ECS `Aura` component.
- **4d** adds `Effect::EnterPhase`. Legacy `phase_transition_system` keeps running in parallel — once engine fires for a given Damage, the legacy system finds nothing pending and is a no-op (idempotency check). Sim now sees revives.
- **4e** is the deletion sweep — high blast radius, but everything it removes has been running dead-code since 4b/4c/4d. Bridge translator gets wired here so engine events finally produce CombatPhase transitions. Manual playtest gate.
- **4f** is cleanup + sim wiring + retrospective. Low risk.

**Why dual-run in 4c/4d instead of cutover?** Engine + legacy run together during the migration window; correctness verified by parity, not by atomic switch. This makes any divergence loud (legacy mutates ECS that engine query then over-reads → assertion fail) instead of silent (engine missed an edge case → manifests three days later in playtest). Cost: ~1 extra commit per area for parity assertions, removable in 4e.

---

## 7. Gate criteria (Phase 4 → Phase 5)

| # | Criterion | Verification |
|---|---|---|
| 1 | Engine `step(Action::EndTurn)` advances `state.turn_queue` correctly: mid-round, end-of-round-wrap, dead-skip, stunned-skip (status-applied AND aura-applied), all-dead-wrap | Engine tests in `turn_queue.rs` |
| 2 | Sirota DoT still ticks when poisoner is skipped (dead-skip recursion path) | Engine test in `end_turn.rs` |
| 3 | Aura `speed_bonus` reflects in MP refill at start of victim's turn (Phase 3.5b regression closure) | Bridge integration test: source with haste-aura adjacent → victim's MP next turn = base+aura_bonus |
| 4 | Aura-stun-on-next-actor skips correctly: source has stun-aura adjacent to next-to-act actor → engine skips at `Effect::AdvanceTurn` without aura projection step | Engine test |
| 5 | Aura combat-log events: moving into haste-aura emits `CombatEvent::StatusApplied`; moving out emits `StatusRemoved` | Bridge integration test |
| 6 | Boss phase trigger fires reactively on Damage: hp crosses threshold → `Effect::EnterPhase` derived; if `heal_to_full`, boss never enters `Dead` state | Engine test in `phase.rs` |
| 7 | Multi-threshold AoE damage fires multiple phases in one `step()` | Engine test |
| 8 | Round wrap fires `Event::RoundStarted` exactly once; `state.round` increments; alive units' `reactions_left` reset; `phase` cycles `ActorTurn → EndRound → PreRound → ActorTurn` | Engine test |
| 9 | `skip_dead.rs`, `auras.rs`, `phases.rs` deleted; grep clean | grep |
| 10 | `advance_turn_system` collapsed to ≤30 lines; wrap-detection tests moved to engine | code review + LOC count |
| 11 | `process_action_system` param count ≤14 after `VisualAssets` bundle | code review |
| 12 | AI sim's `apply_endturn` wired; mining run shows no regression in agenda mix (tolerance: ≤5% shift per agenda category); boss revivals are now visible to AI scoring | Mining run before/after |
| 13 | Player flow: full combat encounter playable end-to-end, including AI turns, multiple rounds, deaths, summons, boss phases (Phase 3.5 carry-over preserved) | Manual playtest of 2 encounters (including 1 with multi-phase boss) |
| 14 | `cargo test` full suite green | CI |

---

## 8. Known gotchas

- **Recursion bound for `Effect::AdvanceTurn`.** If every unit in the queue is dead or stunned (edge case: party wipe + statuses), the recursion is bounded by queue length. Use a counter (`max_steps = state.turn_queue.order.len() + 1`) and break with `Event::RoundStarted` derived even mid-recursion. Test: all-dead queue.
- **Wrap detection vs current `queue.advance() % len`.** ECS-side `TurnQueue::advance` uses modulo, so `index < prev` is the wrap signal. Engine must mirror: `TurnQueue::wrapped_after(prev_idx) -> bool` returns `self.index < prev_idx || self.order.is_empty()`. Test edge case: queue of length 1 — advance always wraps to itself; should `BumpRound` every turn.
- **`Event::RoundStarted` ordering vs `RefreshAggregates`.** `BumpRound` must emit `RefreshAggregates` for all alive units BEFORE `RoundStarted` (so aggregates are fresh when the next turn starts). The effect cascade naturally enforces this because `RefreshAggregates` derived from `BumpRound` lands ahead of `TurnStarted` from `AdvanceTurn` (same step, queue-FIFO).
- **`NextState<CombatPhase>` write from translator.** Bevy's `NextState` requires `ResMut` access. The bridge translator (currently a fn helper) becomes a system-param dependent. Either pass `&mut NextState<CombatPhase>` to `translate_action_events`, or have the system that calls the translator inspect the returned events for `RoundStarted` and set the phase itself. Prefer the latter — keeps the translator pure.
- **`PresetInitiative` clear timing.** `build_turn_order` (now `bridge_start_round`) consumes preset on round 1 and clears. If engine ever issues a `BumpRound` that re-enters round-1 logic (it should not — `state.round == 1` only on bootstrap), preset is already clear. Add assertion: `bridge_start_round` panics if called with `state.round > 1`.
- **Aura diff-on-move snapshot points.** `step()` must snapshot `aura_membership_set` BEFORE EACH `Effect::MovePosition` and after each one — multiple moves in one cascade (chain-AoO scenarios? Push effects?) each emit their own diff. Naive "snapshot once at step-start, diff at step-end" loses intermediate transitions and emits net deltas only. Decide per-effect vs per-step explicitly; per-effect is correct.
- **Aura source moves are also covered by diff-on-move.** Same MovePosition effect applies whether the moving unit is an aura source or an aura target — diff captures both cases since membership is `(target, source, status)` triples. Test: source moves out of all neighbors' radius → multiple `AuraStatusLost` events.
- **Aura death-source semantics.** When `Effect::Death` fires for an aura source, the next `aura_effects_on(target)` query will naturally exclude it (`alive_units()` filter). But the **diff log** needs explicit handling: after `Effect::Death`, snapshot `aura_membership_set` and emit `AuraStatusLost` for the difference. Mitigation: treat `Effect::Death` like `Effect::MovePosition` for the diff — also wrap with snapshot.
- **Phase preempts Death — derived-effect ordering.** `apply_effect(Damage)` must check phase trigger and Death derivation as a 2-stage decision: first compute new_hp, then `if phase_trigger(new_hp) → push EnterPhase` else `if new_hp ≤ 0 → push Death`. **Not** `push EnterPhase if trigger; also push Death if new_hp ≤ 0`. Test the exact case: lethal damage to boss with phase trigger at 50%.
- **Phase trigger on cascading damage.** If `step()` is processing an AoE with 3 damage effects in queue, and a single damage drops the boss across two thresholds (50% and 25%), only one trigger fires per `Damage` effect. The next damage in the same queue fires the next trigger. Tests must cover this.
- **`EnemyPhases.pending` decrement.** Bridge translator must `pop_front()` from ECS `EnemyPhases.pending` on `Event::PhaseEntered`. If engine fires twice (one Damage → trigger; next Damage in queue → next trigger), bridge gets two `PhaseEntered` events in order — `pop_front` twice. Verify ordering preserved end-to-end.
- **AI `apply_endturn` wiring point.** Per Phase 3 retro: the correct hook is "end of plan branch", not "end of each candidate action". For single-step planners, that's once per candidate. For beam search (depth > 1), it's at terminal frontier evaluation only. Wire conservatively — single tick per branch — and add a regression test that double-ticking is not happening.
- **`Res<TurnQueue>` UI consumers.** Grep for `Res<TurnQueue>` across `src/ui/`; confirm all are read-only after Phase 4. The projection writes the engine's queue → Res inside `project_state_to_ecs`. If a UI system writes, it must be rewritten in 4e.
- **Parity assertions in 4c/4d dual-run.** Cross-check engine aura query vs ECS `StatusEffects` aura entries every `engine_turn_start_system` call during the dual-run window. Similarly: after `phase_transition_system` runs, assert `state.unit(boss).max_hp == ecs_max_hp`. Strip these assertions in 4e once legacy paths are deleted.

---

## 9. Retrospective

(Filled at Phase 4 close.)
