# Phase 2 — Cast: damage, heal, status apply

**Parent plan:** `docs/ai/rework/unisim.md` §5.2
**Predecessor:** `docs/ai/rework/step_unisim1_plan.md` (Phase 1 — 5/5 ✅, tagged `unisim/phase1-complete`)
**Goal:** `Action::Cast` is canonical on both real (Bevy runtime) and sim (AI planner). Damage / heal / status apply flow through `combat_engine::step()`. `apply_effects_system`, `sim::apply_cast`, and the `ApplyDamage` / `ApplyHeal` / `ApplyStatus` Bevy messages disappear. ECS `Vital.hp`, `Mana`, `StatusEffects` become read-only projections.
**Timebox:** 3 weeks.

---

## 1. Scope

**IN:**
- New `Action::Cast { actor, ability, target, target_pos }` variant in engine.
- New `Effect` variants: `PayCost`, `Heal`, `ApplyStatus`, `RemoveStatus`. (`Damage`, `GainRage`, `Death`, `RefreshAggregates` already exist.)
- New `Event` variants: `AbilityResolved`, `UnitHealed`, `StatusApplied`, `StatusRemoved`.
- `expand_action(Action::Cast)` reads the `AbilityDef` from `ContentView`, computes affected targets, fans out into one effect per `EffectDef` arm. Per-target ordering (decision 6.3) is now load-bearing: each target's damage → rage → death resolves before the next target.
- `process_action_system` consumes `ActionInput::Cast` alongside `ActionInput::Move`; uses the existing `EcsContentView` adapter (extended with status / heal / cost queries) and `DiceRngAdapter`.
- Bridge event translation extended to write `CombatLog::DamageResult`, `HealResult`, `StatusApplied`, `StatusExpired`, `AbilityUsed`, `ManaChanged` from engine events.
- `project_state_to_ecs` extended to project `Mana.current`, `StatusEffects`, `Dead` marker comprehensively (Move-time Dead inserts stay).
- `apply_effects_system` deleted (130 lines). `resolution.rs::resolve_action_system` writes `ActionInput::Cast` instead of `ApplyDamage`/`ApplyHeal`/`ApplyStatus`.
- `sim::apply_cast` + `sim::apply_primary` + `sim::apply_statuses` deleted. `SimState::apply_step` for `PlanStep::Cast` calls `step()` directly (mirrors Phase 0's `apply_move` shim pattern, now collapsing).
- AI `enumerate_actions` pre-validates target liveness to avoid tripping decision 6.5 (`TargetGone` rollback) on dead targets.
- Pre-Phase-2 + post-Phase-2 playtest snapshots for behaviour-change validation (decisions 6.3 + 6.5).

**OUT (deferred to Phase 3+):**
- `Action::EndTurn` and round-tick mechanics (`TickDot`, status duration decrement) — Phase 3.
- `Action::Cast` for abilities with summon (`EffectDef::Summon`) — `SpawnUnit` stays a Bevy message in Phase 2; summon handling migrates in Phase 3 or 4.
- Crit-fail side effects (`CritFailSideEffect`) — these are content data, but the engine doesn't currently model them. Phase 2 preserves the existing sim/real path for crit-fails via a bridge fallback OR migrates them as engine effects (decide in §8).
- `auras_system` — Phase 3.

---

## 2. Architecture diff vs Phase 1

```
Phase 1 (current):                           Phase 2 (target):
  CombatState (auth: Move state)               CombatState (auth: Move + Cast state)
        │                                            │
        │ projector: pos, hp, mp, reactions, rage    │ projector: + mana, statuses, more dead
        ▼                                            ▼
  ECS components (Move read-only;             ECS components (Move + Cast read-only)
   apply_effects still writes hp/             apply_effects deleted; resolution writes
   mana/statuses for Cast path)               ActionInput::Cast; engine drives Cast
        │                                            │
        ▼                                            ▼
  step(Action::Move) drives every Move         step(Action::{Move,Cast}) drives every
  process_action_system → step()               player/AI action
```

Bridge surface:
- `EcsContentView` gains: `ability_def(AbilityId)`, `weapon_dice(UnitId, AbilityId)` (per-ability not just AoO), `status_def(StatusId)` for cost/duration lookup.
- `ActionInput` enum gains `Cast { actor, ability, target, target_pos }` variant.
- Projector gains `Mana` + `StatusEffects` writes; Dead-marker insertion path generalized.

Sim surface:
- `SnapshotContentView` extends to mirror the same engine `ContentView` surface (already partially there via `aoo_dice` for `SnapshotContentView`).
- `SimState`'s heavy snapshot-based scoring helpers (`apply_primary`, `apply_statuses`, `apply_cast`) collapse into thin `step()` calls on a persistent `CombatState`. Step 7 from Phase 1 (deferred) folds in here naturally.

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/src/action.rs` | Add `Cast` variant + `ActionError::TargetGone` already exists; add `UnknownAbility`, `NotInRange`, `InsufficientCost` |
| `crates/combat_engine/src/effect.rs` | Add `PayCost`, `Heal`, `ApplyStatus`, `RemoveStatus` Effect variants; extend `apply_effect` |
| `crates/combat_engine/src/event.rs` | Add `AbilityResolved`, `UnitHealed`, `StatusApplied`, `StatusRemoved` Event variants; extend `effect_to_event` |
| `crates/combat_engine/src/content.rs` | Extend `ContentView` trait: `ability_def`, `weapon_dice_for_ability`, `status_def`. Add data types: `AbilityDef` (engine-side, minimal), `StatusDef` (engine-side, minimal) |
| `crates/combat_engine/src/step.rs` | Add `Action::Cast` arm to expansion: read ability def, compute targets (delegate to a new `crate::targeting` module), fan out effects per `EffectDef` arm with per-target ordering (decision 6.3) |
| `crates/combat_engine/src/targeting.rs` | New module — engine-side target enumeration (single / AoE circle / cone). Mirrors `crate::combat::effects_state::compute_affected_targets` |
| `src/combat/engine_bridge.rs` | Extend `EcsContentView` with ability/status lookups; extend `process_action_system` to handle `ActionInput::Cast`; extend event translation; extend projector to write `Mana`, `StatusEffects` |
| `src/combat/apply_effects.rs` | **DELETE** (file). Remove pipeline registration + import |
| `src/combat/resolution.rs` | Replace `dmg_writer` / `heal_writer` / `status_writer` writes with `action_input.write(ActionInput::Cast { … })`. The `ValidatedAction` → effects fan-out moves into engine; `resolve_action_system` becomes a thin pass-through |
| `src/combat/validation.rs` | Either delete (engine pre-validates in `step()`) OR keep as a Phase 1.5 transitional gate that pre-checks target liveness for the AI's benefit. Decide in §8 |
| `src/combat/advance_turn.rs` | Remove `ApplyStatus` consumer (status apply now flows through engine event → bridge → ECS) |
| `src/game/messages.rs` | **DELETE** `ApplyDamage`, `ApplyHeal`, `ApplyStatus` structs. `UseAbility` stays (it's the player/AI input). Extend `ActionInput` enum: add `Cast { actor, ability, target, target_pos }` |
| `src/main.rs` | Remove `add_message::<ApplyDamage>` / `ApplyHeal` / `ApplyStatus` |
| `src/combat/ai/plan/sim.rs` | Delete `apply_cast`, `apply_primary`, `apply_statuses` (lines 183-438). `apply_step(PlanStep::Cast)` calls `step()` on persisted `CombatState`. Folds Phase 1 deferred step 7 into this work |
| `src/combat/ai/plan/sim.rs` | `SimState` gains `combat_state: CombatState`; `from_snapshot` builds it once; `apply_move` and `apply_cast` mutate `self.combat_state` directly |
| `src/combat/ai/system.rs` | `enumerate_actions` pre-validates target liveness; AI writes `ActionInput::Cast` instead of `UseAbility` (or keep `UseAbility` as the AI/player input and let `resolve_action_system` convert it) |
| `tests/combat/effects.rs`, `tests/combat/statuses.rs`, `tests/combat/aoo.rs`, `tests/combat/pipeline.rs` | Some assertions on log event ordering may need rewrite per decision 6.3 (all-damages-first → per-target ordering). Audit cost flagged in §8 |
| `tests/combat_engine/` | New tests: `cast.rs` (Cast variants), `targeting.rs` (target enumeration), `status_apply.rs` (effect + event flow). Expand `parity.rs` with cast scenarios |
| `tests/common/mod.rs` | Update fixtures: drop legacy effect messages from test apps; register `ActionInput::Cast` |
| `docs/architecture.md`, `docs/combat-pipeline.md`, `docs/content-guide.md` | Update to reflect engine ownership of damage/heal/status |
| `benches/engine_move.rs` | Add `engine_cast.rs` bench (or extend existing) — gate criterion: ≤ 1.2× Phase 1 baseline (1.81 µs ceiling) |

---

## 4. Implementation order

Land each step as its own commit; CI green before next. Engine extensions land first (additive, no behavior change), bridge translation second (writes alongside `apply_effects`), then the flip + deletion.

1. **Engine: `Effect::Heal` + `Event::UnitHealed`.** Smallest atomic addition. Add to enum, extend `apply_effect`, add `effect_to_event` arm, tests in `tests/combat_engine/effect.rs`. No bridge work yet.
2. **Engine: `Effect::PayCost` + `Effect::ApplyStatus` + `Effect::RemoveStatus`.** Status mutations follow the same enum / apply / event pattern. `ApplyStatus` writes to `Unit.statuses`; `RemoveStatus` filters by id; `PayCost` decrements `Unit.mana` or `Unit.rage` per cost kind. Tests for each.
3. **Engine: `crate::targeting` module + `ContentView` extension.** Target enumeration (single / AoE / cone) ported from `combat/effects_state::compute_affected_targets`. `ContentView::ability_def` returns a minimal engine-side `AbilityDef` (target_type, range, aoe_shape, effect_list — much narrower than the full content `AbilityDef`).
4. **Engine: `Action::Cast` in `step()`.** `expand_action(Cast)` reads ability def, computes affected, fans effects per target with per-target ordering. Strict failure for non-actor `Damage` on dead target stays as decision 6.5. Tests for: single damage, AoE damage with per-target ordering, heal, status apply, cost payment, kill-mid-AoE-rollback.
5. **Bridge: `ActionInput::Cast`.** Add variant, route in `process_action_system`. `EcsContentView` extends to satisfy the new `ContentView` surface. Bridge event translation writes `CombatLog` entries matching today's `apply_effects` output (within decision-6.3 ordering tolerance). **Engine runs as witness** alongside `resolve_action_system` + `apply_effects_system`. Smoke tests in `tests/combat_engine/cast_bridge.rs`.
6. **Projector: `Mana` + `StatusEffects`.** Extend `project_state_to_ecs` to write `Mana.current` (when engine `Unit.mana = Some((cur, max))`) and replace `StatusEffects` contents (write `ActiveStatus { id, rounds_remaining, dot_per_tick, applier: <legacy field> }` from engine `Unit.statuses`). Note: legacy `ActiveStatus` carries an `applier: Entity` field; engine doesn't track applier (decision needed §8).
7. **AI scoring sanity.** Mining run before Phase 2 + after step 5 (bridge witness) confirms agenda mix unchanged. If AI scoring formulas regress (e.g. different damage rounding through engine), pin via test.
8. **Flip authority + delete legacy.** `resolution.rs` writes `ActionInput::Cast`. Delete `apply_effects_system` + its registration. Delete `ApplyDamage` / `ApplyHeal` / `ApplyStatus` messages. Migrate `tests/combat/effects.rs` / `statuses.rs` assertions per decision 6.3. Delete sim's `apply_cast`/`apply_primary`/`apply_statuses`; SimState now persists `CombatState` (fold of Phase 1's deferred step 7).
9. **Bench capture.** `cargo bench --bench engine_cast` (new) + re-run `engine_move`. Gate: combined ≤ 1.2× Phase 1 baseline (1.81 µs for Move).
10. **Mining + playtest snapshot post-Phase-2.** Compare agenda mix, decision distribution. Document drift in retro.

---

## 5. Existing code to consult

| When writing | Read |
|---|---|
| Engine `Action::Cast` + `expand_action` | `src/combat/resolution.rs` (current effect fan-out), `src/combat/effects_outcome.rs::compute_ability_outcome` |
| Targeting module | `src/combat/effects_state.rs::compute_affected_targets`, `src/game/hex.rs` (hex_circle, hex_line, has_los) |
| Heal logic (DoT-neutralize) | `src/combat/apply_effects.rs:73-114` |
| Status apply | `src/combat/advance_turn.rs` (`ApplyStatus` consumer) — locate via `rg "MessageReader<ApplyStatus>"` |
| Status aggregate refresh | `crates/combat_engine/src/effect.rs::Effect::RefreshAggregates` (already implemented for AoO armor_bonus) |
| Bridge event translation pattern | `src/combat/engine_bridge.rs::translate_move_events` (Phase 1 reference) |
| Sim cleanup | Current `src/combat/ai/plan/sim.rs` lines 183-438 (apply_cast + helpers) |
| Per-target ordering pin | `crates/combat_engine/src/step.rs` Move reaction loop (commit `37acf69`) — same per-target derive pattern |

Run `ya tool ast-index outline <file>` before any read > 500 lines.

---

## 6. Test plan

**Engine unit tests (`tests/combat_engine/`):**
- `effect.rs` (extend): `Heal` neutralizes DoT then HP; `PayCost` decrements mana / rage; `ApplyStatus` adds to `Unit.statuses` + triggers `RefreshAggregates`; `RemoveStatus` filters by id.
- `targeting.rs` (new): single, circle radius-1/2, cone, LOS blocked, out-of-bounds clipping.
- `cast.rs` (new): basic damage cast, AoE damage (per-target ordering pinned), heal, status apply with duration, status apply replaces existing, cost payment fails on insufficient.
- `parity.rs` (extend): 4-6 cast scenarios (mirror existing `parity_*_real_vs_sim` style).
- `step.rs` (extend): kill-mid-AoE strict failure (decision 6.5 non-actor path — finally exercised in production).

**Bridge tests (`tests/combat_engine/cast_bridge.rs`):**
- `engine_emits_combat_log_damage_result_for_cast`
- `engine_emits_status_applied_event`
- `engine_heal_cleanses_dot_then_restores_hp`
- `projector_writes_mana_after_cost_paid`
- `projector_writes_status_effects_from_engine`

**Existing tests that may need rewrites (decision 6.3 audit):**
- `tests/combat/effects.rs::apply_damage_reduces_hp` — single-target, no ordering issue.
- `tests/combat/effects.rs::aoe_damages_multiple_enemies` (if asserts log ordering) — likely needs rewrite.
- `tests/combat/effects.rs::rage_gained_on_dealing_and_receiving_damage` — rage ordering changes from "all damages then all rages" to "per-target damage-then-rage". Reassert.
- `tests/combat/statuses.rs::burning_lasts_two_applier_end_turns` — duration mechanic, unaffected (Phase 3 owns ticks).
- `tests/combat/statuses.rs::reapplying_status_replaces_previous` — single-target, no ordering issue.
- Estimated audit: 5-8 tests per `tests/combat/effects.rs` + `statuses.rs` need close reading; ≤3 require actual rewrites.

**Existing tests that must stay green by construction:**
- `tests/combat_engine/parity.rs` (all 4 sim-vs-engine scenarios) — sim now routes through engine for Cast too, so identity holds.
- `tests/combat/aoo.rs` (all 9 movement scenarios) — Phase 2 doesn't touch Move.
- `tests/combat/pipeline.rs` — full pipeline integration, must pass with new flow.

---

## 7. Gates (pass/fail per task list §5.2)

| # | Criterion | Verify |
|---|---|---|
| 1 | Damage / heal / status parity | `cargo test --test combat --test combat_engine` 100% pass |
| 2 | 12.1 (speed + status refresh) preserved | Aura/speed parity tests stay green |
| 3 | 12.3 (rage on damage + AoO) preserved | Rage tests stay green; new per-target ordering test pins post-Phase-2 sequence |
| 4 | AI scoring stable | Mining run pre / post: agenda mix shift ≤ 5%, decision-band distribution shift ≤ 3% |
| 5 | Bench ≤ 1.2× Phase 1 baseline (1.81 µs ceiling) | `cargo bench --bench engine_move --bench engine_cast` |
| 6 | No `apply_effects_system` in source | `ya tool ast-index search "fn apply_effects_system"` empty |
| 7 | No `ApplyDamage` / `ApplyHeal` / `ApplyStatus` types | grep returns empty |
| 8 | No `sim::apply_cast` | grep returns empty |

---

## 8. Risks / flags

**Open — resolve before implementation.** Pre-implementation discovery (TBD before step 1) should answer:

- **Per-target ordering (decision 6.3) — actual test impact.** The plan estimates "≤3 rewrites" but this is a guess. Audit needed: read each `tests/combat/effects.rs` and `tests/combat/statuses.rs` test, classify as (a) ordering-agnostic, (b) ordering-sensitive but compatible with per-target, (c) requires rewrite. Decide before starting step 8.
- **Status `applier` field.** `crate::game::components::ActiveStatus` carries `applier: Entity` for tracking who applied the status (used in DoT damage attribution for kill credit in Phase 3). Engine `combat_engine::ActiveStatus` has no applier. Options: (a) add `applier: UnitId` to engine `ActiveStatus`, (b) defer applier tracking to bridge layer, (c) drop applier (game-design simplification). Decision needed before step 6.
- **Crit-fail side effects (`CritFailEffect`).** Phase 1 has no crit fail — those are content data branched on dice rolls. Engine doesn't currently model them. Phase 2 options: (a) add `CritFailEffect` to engine (extra scope), (b) keep crit-fail handling in `resolution.rs` as a pre-step that emits standard effects before `ActionInput::Cast`, (c) move to Phase 3 alongside DoT.
- **`UseAbility` vs `ActionInput::Cast` boundary.** `UseAbility` is the player/AI INPUT message; `ActionInput::Cast` is what the bridge consumes. Two natural designs: (1) AI/player writes `UseAbility`; `resolve_action_system` validates + translates to `ActionInput::Cast`. (2) AI/player writes `ActionInput::Cast` directly; resolution disappears. Decision needed before step 8.
- **`validate_action_system` future.** Currently validates: actor liveness, AP cost, mana cost, range, taunt rules, target validity, status disadvantage rules. Engine's `step(Action::Cast)` validates a SUBSET (cost + target liveness + range). Either: (a) keep `validate_action_system` as a pre-engine gate for the UX-friendly rejections (range, taunt) — engine validation becomes a backstop. (b) Move all validation into engine; engine returns rich `ActionError` enum; UI renders rejection reasons from errors. Plan-impact significantly differs.
- **Sim `combat_state` persistence semantics.** Phase 1 step 7 deferred this. In Phase 2, SimState must hold `CombatState` for Cast scoring. Open: how does the AI planner clone SimState across branches? `CombatState` clone is O(N units). For 100 branches × 10 units, that's 1000 unit-clones per AI tick. Profile early in step 8.
- **AoE damage event count.** Current `apply_effects_system` emits one `DamageResult` log entry per target. Engine emits one `Event::UnitDamaged` per target. Bridge translation: 1-to-1 mapping. But the engine emits `Event::UnitDamaged` BEFORE the same target's `Event::RageGained` (per-target ordering); current log has all DamageResults then all RageGained (all-damages-first ordering). Audit: do any log readers (replay, mining, tests) depend on the old grouping?
- **`Effect::RefreshAggregates` triggering on status apply/remove.** Currently `Effect::RefreshAggregates` runs implicitly after AoO damage (Phase 1, for armor_bonus from new statuses). Phase 2: `ApplyStatus` and `RemoveStatus` must derive a `RefreshAggregates` to keep speed/armor_bonus current. Add to `Effect::apply_effect` arm.
- **`auras_system` interaction.** Auras apply/remove statuses every frame in `CombatStep::TurnStart`. Phase 2 doesn't migrate auras (Phase 3 work), but `auras_system` mutates `StatusEffects` — and the projector now wants to write `StatusEffects` too. Risk of write-write conflict per frame. Need to verify order: auras run TurnStart, projector runs after process_action in Execute. Probably fine but verify.
- **`SpawnUnit` from `EffectDef::Summon`.** Currently `resolve_action_system` writes `SpawnUnit` from a Summon ability. Engine has no spawn primitive in Phase 2 scope. Options: (a) keep `SpawnUnit` Bevy message + `apply_spawn_system`, with `resolve_action_system` extracting summon arms before converting rest to `ActionInput::Cast`. (b) Migrate summon to engine in Phase 2 (extra scope). Recommend (a).

---

## 9. Rollback

- Revert from `unisim/phase2-complete` tag back to `unisim/phase1-complete`. Behaviorally clean revert — Phase 1 is the last known-good gate. Engine code stays (additive), bridge / sim deletions roll back.

Spike-style throwaway is not possible; Phase 2 is production path.

---

## 10. Items to flag back

Stop and ask before proceeding if:

- **Per-target ordering audit surfaces > 5 test rewrites.** May indicate decision 6.3's scope was underestimated — revisit per-target vs all-damages-first.
- **AoO test (`tests/combat/aoo.rs`) starts failing.** Phase 2 shouldn't touch Move; failure indicates accidental shared-code regression.
- **AI mining drift > 10% on agenda mix.** Indicates engine cast resolution diverges from sim's scoring assumptions; pin the formula divergence before continuing.
- **Bench regression > 1.5× Phase 1 baseline** — profile before optimizing; likely culprit is per-target ordering increasing apply count, or `CombatState` clone cost in sim hot loop.
- **`validate_action_system` decision** crosses 2-day investigation — escalate; the choice is design-level and affects step 8 substantially.
- **`auras_system` write-write conflict on `StatusEffects`** — surface before step 6.

---

## 11. Done = merged + gates + retro

After gates pass:
1. Append `## 12. Retrospective` with surprises, deviations, perf numbers, decisions for Phase 3.
2. Open `step_unisim3_plan.md` from §5.3 template in `unisim.md`.
3. Tag commit `unisim/phase2-complete`.
