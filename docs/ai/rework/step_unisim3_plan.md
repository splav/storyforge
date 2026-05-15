# Phase 3 — Status ticks, DoT, `Action::EndTurn`

**Parent plan:** `docs/ai/rework/unisim.md` §5.3
**Predecessor:** `docs/ai/rework/step_unisim2_plan.md` (Phase 2 — tagged `unisim/phase2-complete`)
**Goal:** End-of-applier-turn DoT ticks + status duration decrement migrated into `combat_engine`. `status_tick_system` removed. `Action::EndTurn` introduced (minimal — heavy turn-queue work stays in Phase 4). AI sim simulates ticks via engine call, closing today's "sim skips DoT" drift. `Effect::Death` becomes a status-cleanup boundary.
**Timebox:** ~2 weeks.

**Out of scope (deferred):**
- `EffectDef::Summon` engine modeling — owned by **Phase 3.5** (Summon migration as its own step).
- Turn queue / round flow / `RoundPhase` — Phase 4.
- HexPositions sync panic (orthogonal bug, separately tracked).

---

## 1. Scope

**IN:**

**Engine extensions:**
- `Effect::TickDot { target, status }` — atomic: derives `Damage { target, raw, source: applier, pierces: false }` and `Damage { target, raw, source: applier, pierces: true }` for `dot_per_tick` and `hp_percent_dot` respectively. Damage cascade reuses Phase 2 plumbing (rage × 2, death).
- `Effect::ExpireStatus { target, status }` — atomic: decrement `rounds_remaining` by 1; if 0 → remove status, derive `RefreshAggregates`.
- `Effect::Death` cleanup — derive `RemoveStatus { target: unit, status }` for every status on the dying unit (behavior improvement; sirota-DoTs on *other* units survive because their `applier == dead_unit` keeps ticking via engine's start-of-turn hook).
- `Event::StatusTicked { target, status, source }` — fires before the `UnitDamaged` cascade so log can render "поражён ядом" then damage breakdown.
- Engine method `CombatState::start_actor_turn(actor) -> Vec<Event>` extended (Phase 2.5 left it doing AP/mana/energy regen only): now also fans out `TickDot` + `ExpireStatus` for every status across all units where `applier == actor`. Works for dead actors too — closes sirota-DoT via engine, no special-casing in bridge.
- `Action::EndTurn { actor }` variant + engine arm in `step()`. Phase 3 keeps it minimal: emits `ActionStarted` + `ActionFinished`. No effects today. Reserved for Phase 4 to do queue advance + RoundPhase transitions.

**Bridge / projector:**
- `engine_turn_start_system` (the existing handoff detector) reads the larger event vec from `start_actor_turn` and translates new variants:
  - `Event::StatusTicked` → `CombatEvent::PoisonTick`
  - `Event::StatusRemoved` (from ExpireStatus path) → `CombatEvent::StatusExpired` (already wired by Phase 2)
  - Damage / Death / RageGained — already wired by Phase 2's `translate_cast_events` pattern; refactor into a shared helper (`translate_engine_events`) or inline.
- New `ActionInput::EndTurn { actor }` Bevy message (already exists as `EndTurn` message — rename / consolidate). Bridge's `process_action_system` routes it to `step(Action::EndTurn)`. Engine event stream translated.
- `apply_effect(Effect::TickDot)` requires lookup of `dot_per_tick` and `hp_percent_dot` for a status. These live on `StatusDef`. `ContentView::status_def` already exposed in Phase 2. Engine reads from `ContentView`.
- `apply_effect(Effect::Death)` now emits `RemoveStatus` derived effects per local status — `apply_effect(RemoveStatus)` already handles `RefreshAggregates`. Free reuse.

**AI sim:**
- `sim::apply_endturn(actor)` calls `step(state, Action::EndTurn { actor }, &mut ExpectedValue, content)` after the actor's primary action plan resolves. Sim's score function now sees post-tick state. Closes "sim doesn't tick" drift.
- For multi-step beam search: each frontier expansion applies EndTurn for the planning actor before scoring (one tick per planned turn).

**Deletions:**
- `src/combat/status_tick.rs` (~30 lines) — file removed.
- `src/combat/advance_turn.rs::tick_status_durations` + `tick_statuses_on_entity` + `TickResult` (~80 lines) — sirota-DoT case now handled by engine when `advance_turn` issues `ActionInput::EndTurn` for the dead-but-skipped actor.
- The corresponding `pub mod status_tick;` line in `combat/mod.rs`.

**Pipeline change:**
- TurnStart chain: `engine_turn_start_system` already does refills; after Phase 3 it also fans out tick events. No new system here.
- Finalize chain: `advance_turn_system` emits `ActionInput::EndTurn` BEFORE advancing queue (and for each dead-skip pass).

**OUT (deferred):**
- `EffectDef::Summon` migration → Phase 3.5.
- `auras_system` migration → Phase 4 (still ECS in Phase 3; projector's applier-aware merge preserves aura entries as before).
- Real turn-queue / RoundPhase / round transitions in engine → Phase 4.

---

## 2. Architecture diff vs Phase 2

```
Phase 2:                                      Phase 3 target:
  start_actor_turn(actor):                      start_actor_turn(actor):
    refill AP                                     refill AP
    +1 mana (clamped)                             +1 mana (clamped)
    +1 energy (clamped)                           +1 energy (clamped)
                                                  + fanout TickDot per (target, status) where applier==actor
                                                  + fanout ExpireStatus per same set

  status_tick::tick_status_effects_system       (DELETED — covered by start_actor_turn)
   runs in CombatStep::TurnStart                 

  advance_turn::tick_status_durations           (DELETED — engine handles sirota tick when
   for dead-skip loop                            advance_turn issues EndTurn for dead actor)

  Effect::Death { unit }                        Effect::Death { unit }:
    hp = 0, no cleanup                            hp = 0
                                                  + derive RemoveStatus { target: unit, status } × N
                                                    (cleans local statuses; sirota statuses preserved)

  Action variants: Move, Cast                   Action variants: Move, Cast, EndTurn
                                                  (EndTurn is a thin marker in Phase 3; carries
                                                   no effects; engine emits ActionStarted/Finished)

  ECS still authoritative for tick fanout       Engine sole owner of ticks
  AI sim skips ticks → drift                    AI sim calls step(EndTurn) → tick-aware scoring
```

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/src/effect.rs` | Add `Effect::TickDot { target, status }`, `Effect::ExpireStatus { target, status }`; `apply_effect` arms; `Effect::Death` derives `RemoveStatus` per local status |
| `crates/combat_engine/src/event.rs` | Add `Event::StatusTicked { target, status, source }`; update `effect_to_event` for new effects |
| `crates/combat_engine/src/action.rs` | Add `Action::EndTurn { actor }` variant |
| `crates/combat_engine/src/step.rs` | `Action::EndTurn` arm: legality (actor exists; no failure modes today), emit `ActionStarted` / `ActionFinished` |
| `crates/combat_engine/src/state.rs` | `CombatState::start_actor_turn(actor)` extended: append TickDot / ExpireStatus events from helper `fn fanout_ticks_for_applier(state, applier, content) -> Vec<Effect>` |
| `crates/combat_engine/src/content.rs` | `StatusDef` already has `hp_percent_dot` (verify; add if missing) — engine reads via `ContentView::status_def` |
| `src/combat/engine_bridge.rs` | `engine_turn_start_system` walks the expanded event vec, translates `StatusTicked` → `CombatEvent::PoisonTick`, damage cascade events into log. `process_action_system` adds `ActionInput::EndTurn` route |
| `src/game/messages.rs` | `EndTurn` becomes / unifies with `ActionInput::EndTurn { actor }` (or stays separate — see decision D6). Player + AI writers updated if shape changes |
| `src/combat/advance_turn.rs` | Strip `tick_status_durations`, `tick_statuses_on_entity`, `TickResult`. The dead-skip loop now writes `ActionInput::EndTurn { actor }` for each dead actor it skips |
| `src/combat/status_tick.rs` | **DELETED** |
| `src/combat/mod.rs` | Remove `pub mod status_tick;` |
| `src/combat/pipeline.rs` | Remove `status_tick::tick_status_effects_system` from `CombatStep::TurnStart` chain |
| `src/combat/ai/plan/sim.rs` | Add `SimState::apply_endturn(actor)` — single line: call `step(EndTurn)` against `self.combat_state` |
| `tests/combat_engine/effect.rs` | Add tests: TickDot → damage + rage cascade; ExpireStatus removes at 0; Death cleans local statuses; sirota tick after applier death |
| `tests/combat.rs` (integration) | Adapt any ticking tests; DoT death scenarios |
| `docs/ai/rework/step_unisim3_plan.md` | This file |
| `docs/ai/rework/unisim.md` | Append Phase 3 retrospective in §5.3 after gate |

---

## 4. Sub-step decomposition

Each sub-step lands independently with `cargo check --all-targets` clean and tests green.

| Step | Title | What lands |
|---|---|---|
| **3a** | Engine `TickDot` + `ExpireStatus` effects | New effect variants, `apply_effect` arms, unit tests. No bridge wiring yet — effects are unreachable from `step()`. |
| **3b** | `Death` cleanup of local statuses | `apply_effect(Death)` derives `RemoveStatus` per status on dying unit. Test: dying unit's statuses gone post-step; other units' statuses applied by deceased unit survive (sirota case). |
| **3c** | `Event::StatusTicked` + `effect_to_event` | Event variant + mapping. No bridge translation yet (engine-only enrichment). |
| **3d** | `start_actor_turn` fans out ticks | Engine method extended; new helper `fanout_ticks_for_applier(state, applier, content) -> Vec<Effect>` iterates state.units(), for each unit's status with `applier == X` emits `TickDot` (if has DoT/percent-dot) + `ExpireStatus`. Returns combined effects; existing AP/mana/energy regen runs first; tick effects appended and processed through the engine queue model (or the helper returns effects and caller processes them via a mini step-like loop). Tests: status with `dot_per_tick > 0` reduces target's hp; `hp_percent_dot` applies percent; status with `rounds_remaining == 1` is removed after tick. |
| **3e** | Bridge: translate StatusTicked + tick damage events | `engine_turn_start_system` updated; new events become `CombatEvent::PoisonTick` / `DamageResult` / `StatusExpired` / `UnitDied`. Existing `translate_cast_events` event-arm logic extracted into shared `translate_engine_events(events, source_action) -> Vec<CombatEvent>` helper to avoid duplication. |
| **3f** | `Action::EndTurn` + `ActionInput::EndTurn` routing | Engine arm; bridge route. Bridge emits `ActionInput::EndTurn` from `advance_turn_system` (Finalize, before queue.advance). Engine processes — Phase 3 minimal arm, just ActionStarted/Finished. |
| **3g** | Delete `status_tick.rs` + `tick_status_durations` | Pipeline change + file removals. Sim's `apply_endturn` lands. Parity tests adapted. Phase 3 retrospective in unisim.md. Tag `unisim/phase3-complete`. |

---

## 5. Decisions (locked)

### D1. Tick timing — at start of applier's turn
Engine's `start_actor_turn(actor)` fans out ticks. Matches current ECS semantics ("applier wakes up, their poisons tick on victims"). Doesn't shift behavior. Alternative (RoundPhase::EndRound — per unisim.md §5.3 text) would be a behavior change; declined.

### D2. Atomic effect granularity — TickDot + ExpireStatus separate
Two effects, two responsibilities. TickDot derives Damage (cascades into rage / death). ExpireStatus decrement-or-remove-then-RefreshAggregates. Matches Phase 2 atomicity pattern (PayCost vs Damage separate).

### D3. DoT damage source attribution
`Effect::TickDot { target, status }` reads `status.applier` from `state.unit(target).statuses` (filter by id), derives `Effect::Damage { source: applier }`. Rage on damage (Phase 2 cascade) attributes to the applier — semantically the poisoner gets credit. If applier is dead, the GainRage on dead source is a no-op (Rage clamps).

### D4. Death cleans local statuses; preserves sirota
`Effect::Death { unit }` derives `Effect::RemoveStatus { target: unit, status: s.id }` for each status on the dying unit. Statuses where `applier == dying_unit` but `target != dying_unit` (e.g., poisons the dying unit applied to others) are untouched — those live on their actual targets. The targets' next `start_actor_turn` won't tick them (applier is dead), so engine routes via the dead-actor EndTurn path in step 3f. ✓

### D5. Sirota-DoT model via dead-actor EndTurn
When `advance_turn_system` skips a dead actor, it issues `ActionInput::EndTurn { actor: dead }` to engine. Engine doesn't care about actor liveness for EndTurn; `start_actor_turn(dead)` (which runs at applier-side TurnStart, NOT here) would normally fan out. To handle sirotas in Phase 3, **the fanout is moved from `start_actor_turn` to a new engine helper called by both TurnStart and the dead-skip EndTurn path**, OR we accept dead-skip uses TurnStart-equivalent logic.

Cleaner: introduce `CombatState::tick_actor_statuses(actor) -> Vec<Event>` as the single fanout method. `start_actor_turn(actor)` calls it after refills. Dead-skip in bridge calls it directly (no refills for dead actor). Both share the same engine method.

Decided: **factor tick into its own method** (`tick_actor_statuses`), called by both alive (after refill in `start_actor_turn`) and dead (directly from bridge dead-skip loop) paths.

### D6. `ActionInput::EndTurn` shape
Current `game::messages::EndTurn { actor: Entity }` already exists. We make it a variant of `ActionInput` for consistency with Move/Cast routing:
```rust
pub enum ActionInput {
    Move { actor, path },
    Cast { actor, ability, target, target_pos },
    EndTurn { actor },                  // NEW in Phase 3
}
```
Existing standalone `EndTurn` message gets removed (or kept as legacy alias during transition — see step 3f rollout). Callers (advance_turn, process_action_system) updated.

### D7. AI sim integration
`SimState::apply_endturn(actor)` is a one-liner:
```rust
pub fn apply_endturn(&mut self, actor: UnitId) {
    let _ = step(&mut self.combat_state, Action::EndTurn { actor }, &mut ExpectedValue, &self.content_view);
    // ignore Vec<Event>; sim cares about state, not log
}
```
Beam search expansion: after computing a candidate action's state delta via `apply_move` / `apply_cast`, call `apply_endturn(actor)` to advance ticks. Scoring reads post-tick state.

---

## 6. Sub-step kickoff order

Strict order: 3a → 3b → 3c → 3d → 3e → 3f → 3g.

Each sub-step:
1. `cargo check --all-targets` green.
2. Sub-step's targeted tests green.
3. Full suite (`cargo test`) green.
4. Commit with `ai/unisim Phase 3 step Nx: <title>` (mirror Phase 2 commit style).
5. User review before next sub-step.

---

## 7. Gate criteria (Phase 3 → Phase 3.5)

| # | Criterion | Verification |
|---|---|---|
| 1 | DoT damage parity vs Phase 2 baseline | Golden trace from a poison scenario; engine output byte-equivalent |
| 2 | Sirota-DoT preserved (poisoner dies, victim still loses HP next round) | New parity test |
| 3 | Status duration parity (status with 2 rounds expires on second tick) | Existing tests adapted |
| 4 | Death cleans local statuses; sirotas untouched | New parity test |
| 5 | AI sim scores tick-aware plans (regression: ranged kiting agendas should improve, or at minimum not regress) | Mining run before/after; agenda mix delta logged |
| 6 | `status_tick.rs` deleted, no orphan references | `cargo check` |
| 7 | `tick_status_durations` deleted, sirota path goes through engine | grep for `tick_status_durations` returns empty |
| 8 | `cargo test` full suite green | CI |
| 9 | No playtest regression on existing combat encounters | Manual run of 2 encounters |

---

## 8. Known gotchas

- **Tick order in `start_actor_turn`:** refills run BEFORE ticks, so a DoT-killed actor still gets their AP refilled (visible in state for one frame before projector overwrites HP). Acceptable — ticks happen at the *applier's* TurnStart; the actor who became active is the applier, and their HP isn't the one ticking unless they applied a DoT to themselves (rare).
- **`StatusTicked` event placement vs `UnitDamaged`:** emit `StatusTicked` BEFORE the cascade. Renderer can pair them by source/target. If multiple DoTs hit the same victim in one tick batch, log order is by status iteration order (deterministic).
- **`hp_percent_dot` rounding:** ECS uses `(max_hp * pct + 99) / 100` (ceil). Engine must use the same formula. Add unit test.
- **Aura-applied statuses:** auras live in ECS-only (Phase 4). Projector's applier-aware merge already preserves them. Engine ticks only statuses present in `state.unit.statuses` — auras still get applied per round via `apply_auras_system` in TurnStart, and the projector merges. Should "just work" but verify with a haste-aura scenario.
- **EndTurn for dead actors with no statuses applied:** engine emits ActionStarted/Finished, otherwise no events. Bridge no-op. Confirm no log spam.
- **Heal already neutralizes DoT (Phase 2 `Heal` arm):** that's a *cure*. TickDot is independent — fires while DoT is active. No interaction.

---

## 9. Retrospective

**Steps taken vs planned:**
- 3a: `Effect::TickDot`, `Effect::ExpireStatus`, `Event::StatusTicked` — as planned.
- 3b: `Effect::Death` cleans local statuses — as planned.
- 3c: `Event::StatusTicked` before damage cascade, `ExpireStatus` refactor — as planned.
- 3d: `CombatState::start_actor_turn` fans out ticks; sirota-DoT verified via engine unit test `start_actor_turn_for_dead_applier_still_ticks_sirota`.
- 3f: `Action::EndTurn { actor }` variant + minimal engine arm — as planned (no effects, reserved for Phase 4).
- 3g (this step): sirota-DoT migration to engine in dead-skip loop, ECS tick code deleted, `status_tick.rs` deleted, `translate_tick_events` shared helper extracted, `apply_endturn` defined on `SimState`.

**Plan deviations:**
- `translate_tick_events` was placed in `engine_bridge.rs` (not a new `engine_translate.rs` module) — cleaner, less churn.
- `apply_endturn` in `SimState` calls `tick_actor_statuses` directly rather than going through `step(Action::EndTurn)` — `Action::EndTurn` is minimal (no ticks) in Phase 3; direct call gives the same engine semantics without needing to wire `ActionInput::EndTurn` into the sim.
- `apply_endturn` is defined but **not wired to any call site**. Call sites (beam search expansion, single-step scoring) were inspected; the planner scores after `apply_step` but the correct hook point (after the last step per plan branch, not per step) requires Phase 4 turn-queue context to determine "is this the actor's last step?". Wiring prematurely risks double-ticking. Deferred to Phase 4 with the method ready.

**Surprises:**
- `AooRow` was a private type alias — needed `pub(crate)` to let `advance_turn.rs` use it for `build_ecs_content_view`. Minor.
- `engine_turn_start_system`'s inline mana/energy handling had to be separated from the tick translation loop (the helper doesn't know the actor entity for `ManaChanged`/`EnergyChanged`). Handled by a pre-pass loop for resource events, then `translate_tick_events` for the tick sub-stream.

**Drift closures verified:**
- Sirota-DoT (dead poisoner's statuses still tick on living victims): engine unit test `start_actor_turn_for_dead_applier_still_ticks_sirota` green; the dead-skip loop in `advance_turn_system` now calls `tick_actor_statuses` instead of the deleted ECS path.
- "Sim skips DoT" drift: `apply_endturn` method present. Actual scoring impact deferred (not wired); no regression observed in existing sim parity tests.

**Deletions:**
- `src/combat/status_tick.rs` — gone.
- `TickResult` enum, `tick_status_durations`, `tick_statuses_on_entity`, `percent_dot_damage` — gone from `advance_turn.rs`.
- 8 unit tests covering the deleted ECS functions — gone. Engine-side equivalents in `tests/combat_engine/` cover the same semantics.
- `status_tick::tick_status_effects_system` removed from `CombatStep::TurnStart` pipeline chain.

**Test count:** 816 lib + 43 combat + 119 combat_engine + 1 golden = 979 total. All green.

**Carry-overs to Phase 4:**
- Wire `apply_endturn` call site in beam search / scoring once Phase 4 establishes when a plan branch ends.
- `Action::EndTurn` engine arm grows: turn queue advancement, `RoundPhase` transitions.
- Summon migration → Phase 3.5 (unchanged, as planned).

**Tag:** `unisim/phase3-complete` — to be applied by user.
