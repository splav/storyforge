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

Updated 2026-05-14 after pre-implementation discovery pass.

### Pre-implementation findings

**Per-target ordering (decision 6.3) — RESOLVED.** Audited 28 tests in `effects.rs` (13), `statuses.rs` (13), and `pipeline.rs` (2): **all are end-state assertions** (final HP, status presence, victory flag). None assert on log event order or relative positioning of damage / rage / death entries within an action. Log readers (`replay_assert.rs`, `replay_ai_log.rs`, `mine_ai_logs.rs`) also have no within-action ordering dependencies — they aggregate per-actor stats and decision-kind histograms. **Zero existing tests need rewrite.** Phase 2 still needs to ADD per-target-ordering pin tests for the new behavior (e.g., `aoe_damage_per_target_resolves_before_next`); these go in `tests/combat_engine/cast.rs`. The plan's initial "≤3 rewrites" estimate was conservative.

**Status `applier` field — DECISION: add to engine.** Audited 5 read sites + 2 write sites: `advance_turn.rs:257,282` (DoT tick attribution, expiry tracking), `auras.rs:76,81,91,102` (aura cleanup distinguishes aura-applied from ability-applied + prevents duplicate stacking). All are Phase 2-critical via the status migration. Engine `combat_engine::state::ActiveStatus` gains `applier: UnitId` field; bridge sets it from `entity_to_uid(ApplyStatus.source)`. Adds one `UnitId` per active status — negligible memory.

**Crit-fail side effects — DECISION: pre-step in resolution.rs.** Audited `src/content/races.rs:16` (CritFailEffect enum: Miss, ManaOverload, BrokenFaith, CircuitBreach, Exhaustion, PactControl — 6 variants). All evaluate via `effects_outcome::map_crit_fail` (line 187). Every variant's mechanic maps to existing engine effects (`Damage` for self-damage variants; `ApplyStatus` for status variants). No engine extension needed. `resolution.rs` rolls the crit-fail die before writing `ActionInput::Cast` and emits the appropriate auxiliary effect(s); the main cast continues with potentially-modified cost (ManaOverload doubles). Keeps crit-fail logic localized to the player-facing resolution layer instead of bloating the engine.

**`UseAbility` vs `ActionInput::Cast` boundary — DECISION: option (1) AI/player writes `UseAbility`; resolution translates.** Survey: `UseAbility` has 4 producers (`command_input.rs:102,192` player commands; `ui/hex_grid/input.rs:163,169` player clicks) and 1 consumer (`validation.rs:25`). `validate_action_system` is a thin Bevy adapter (75 lines) over `check_legality()` (`src/combat/actions/mod.rs:156`). AI's `generate_plans` (`src/combat/ai/plan/generator.rs:152`) **already calls `check_legality()` against the snapshot**, so player and AI use one rule layer. Rather than migrating 4 player producers to write a new message type, Phase 2 keeps `UseAbility` as the input message and has resolution route to `ActionInput::Cast`. `validate_action_system` stays (see next finding).

**`validate_action_system` future — DECISION: keep as pre-engine UX gate.** `IllegalReason` has 16 variants (`UnknownActor`, `ActorDead`, `UnknownAbility`, `AbilityNotInList`, `NotEnoughAp`, `InsufficientResource`, `BlockedByStatus`, `OutOfRange`, `TargetOutOfBounds`, `SelfOnlyTargetMismatch`, `WrongTargetTeam`, `TauntForcesTarget`, `TargetUnknown`, `TargetDead`). All are engine-replicable *in principle*, but each carries UX context (tooltips, rejection messages, log entries) that the engine's coarse `ActionError` enum doesn't preserve. Migrating UX rejection to engine errors would require expanding `ActionError` to a 16-variant enum mirroring `IllegalReason` — pure churn for no functional gain. Phase 2 keeps `validate_action_system` as a pre-engine gate that emits structured rejections; engine validation becomes a backstop catching only the strictly-engine-relevant subset (`UnknownActor`, `NoPath`, `OutOfMP`, `TargetGone`). The two layers share the legality contract via the existing `check_legality()` function.

**Sim `combat_state` persistence — DEFERRED clone-cost profile to step 8.** The implementation plan has SimState holding a persistent `CombatState`. Clone cost is O(N units) per branch fork in the AI planner. Rough estimate for 100 branches × 10 units: 1000 unit clones / AI tick. Each `Unit` is ~10 i32 fields + a `Vec<ActiveStatus>` (typically 0-3 entries). Should be sub-millisecond per tick on modern hardware. **Will measure during step 8 implementation; if > 2ms profile, consider `im::Vector<Unit>` (persistent collection) or `Arc<Unit>` cell pattern.** Flag stays open until measured.

**AoE event ordering audit — RESOLVED (subsumed by per-target ordering audit above).** No log reader (replay, mining, tests) depends on within-action event grouping. Per-target ordering is a free behavior change for observers.

**`Effect::RefreshAggregates` after status apply/remove — DECISION: derived in `apply_effect`.** Phase 2's `apply_effect` for `ApplyStatus` and `RemoveStatus` returns `[RefreshAggregates { unit: target }]` as a derived effect. Pattern matches the existing AoO armor_bonus refresh. Engine test pins this in `tests/combat_engine/effect.rs`.

**`auras_system` interaction with `StatusEffects` projection — DECISION: pipeline order is safe in Phase 2, but bridge needs merge logic.** Pipeline order: `apply_auras_system` runs in `CombatStep::TurnStart`; `process_action_system` + `project_state_to_ecs` run in `CombatStep::Execute` (later). Mirror (`init_state_from_ecs`) runs on `OnEnter(AwaitCommand)`, BEFORE `TurnStart`. Risk: engine state at Execute time does NOT include auras that the current frame's TurnStart just applied. If the projector replaces ECS `StatusEffects` with engine state, it clobbers the fresh aura. **Mitigation:** projector merges rather than replaces — for each unit, keep aura-applied entries (identified by `applier` field matching a known aura source) in ECS, replace ability-applied entries from engine. Adds ~10 lines to projector; tested via a new bridge_smoke test (`aura_status_survives_projection_after_unrelated_cast`).

**`SpawnUnit` from `EffectDef::Summon` — DECISION: keep as Bevy message in Phase 2.** `apply_spawn_system` is 163 lines, self-contained. Summoned units join NEXT round (not the current one), so engine state coherence for the current action isn't broken if we skip summons. `resolution.rs`'s Summon arm continues writing `SpawnUnit` alongside `ActionInput::Cast` for other effect arms. AI plan scoring is already summon-unaware, so no scoring drift. Summon migration moves to Phase 3 or 4.

- **SimState clone cost on `CombatState`** — to be profiled during step 8 implementation. If > 2 ms / AI tick, consider `im::Vector<Unit>` or `Arc<Unit>` rather than full clone. Threshold gate: agenda-mix shift > 5% post-Phase-2 vs pre-Phase-2 mining run.
- **AI scoring formula stability** — engine cast resolution must produce identical damage/heal numbers to sim's `compute_ability_outcome`. Pre-Phase-2 mining baseline captures the current agenda mix; post-step-5 (witness-mode) mining run confirms numerical parity before authority flip in step 8.

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
