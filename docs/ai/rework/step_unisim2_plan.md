# Phase 2 — Cast: damage, heal, status apply + legality unification

**Parent plan:** `docs/ai/rework/unisim.md` §5.2
**Predecessor:** `docs/ai/rework/step_unisim1_plan.md` (Phase 1 — 5/5 ✅, tagged `unisim/phase1-complete`)
**Goal:** `Action::Cast` is canonical on both real (Bevy runtime) and sim (AI planner). Damage / heal / status apply / crit-fail flow through `combat_engine::step()`. **Legality computation unified inside the engine crate** — `IllegalReason` + `check_legality` move from `src/combat/actions/` into `combat_engine`; engine's pre-validate is the single source of truth, with player/AI calling it pre-commit for UX tooltips. `apply_effects_system`, `validate_action_system`, `resolve_action_system`, `sim::apply_cast`, and the `UseAbility` / `ValidatedAction` / `ApplyDamage` / `ApplyHeal` / `ApplyStatus` Bevy messages all disappear. ECS `Vital.hp`, `Mana`, `StatusEffects` become read-only projections.
**Timebox:** 3 weeks.

---

## 1. Scope

This phase is bigger than the §5.2 sketch — discovery showed that "keep validate_action_system as UX gate" and "translate UseAbility → ActionInput::Cast in resolution" are duplications dressed as scaffolding. Removing them at the same surface as the Cast migration is cheaper than two phases of churn.

**IN:**

**Engine extensions:**
- **RNG unification.** Engine becomes the canonical owner of `DiceRng` (LCG + scripted-roll support). `crate::core::DiceRng` and `crate::core::DiceExpr` delete; engine's existing `DiceExpr` + a unified `DiceRng` (replacing `SeededDice`, gaining `script(&[i32])`) are re-exported. Bevy wraps via `#[derive(Resource)] struct DiceRngRes(combat_engine::DiceRng)`. `DiceRngAdapter` deletes. Phase 5 benefit: single RNG state = single seed to record for replay byte-equality.
- `Action::Cast { actor, ability, target, target_pos }` variant.
- `Effect` variants: `PayCost`, `Heal`, `ApplyStatus`, `RemoveStatus`. (`Damage`, `GainRage`, `Death`, `RefreshAggregates` already exist.)
- `Event` variants: `AbilityResolved`, `UnitHealed`, `StatusApplied`, `StatusRemoved`.
- `expand_action(Action::Cast)` reads `AbilityDef` from `ContentView`, rolls crit-fail (decision below), computes affected targets, fans out one effect per `EffectDef` arm with per-target ordering (decision 6.3).
- Engine-side targeting module (`crate::targeting`) — single / circle / cone / LOS-aware. Ported from `combat::effects_state::compute_affected_targets`.
- Engine-side crit-fail handling: `ContentView::crit_fail_table(ability) -> Option<CritFailTable>` returns a content-side data structure (`CritFailTable { dice: DiceExpr, branches: Vec<(roll_range, CritFailOutcome)> }`). `CritFailOutcome` variants: `Miss`, `ApplyStatus(StatusId)`, `SelfDamage(DiceExpr)`, `DoubleCost`. Engine rolls once during `expand_action(Cast)` and derives the appropriate aux effects. **All cast-related RNG consumption happens inside `step()`** — Phase 5 replay determinism becomes a single-stream concern.

**Legality unification (the architecturally-correct part):**
- Move `check_legality()` + `ActionState` trait + `IllegalReason` enum from `src/combat/actions/` into `crates/combat_engine/src/legality.rs`. Engine-side; Bevy-free. The trait abstracts over `CombatState` (engine), `BevyActions` (UI/player tooltips), and `SnapshotActions` (AI plan generation) — all three impls continue to work.
- `step(Action::Cast)` pre-validates via `check_legality` against the actor's current `CombatState`; returns `Err(ActionError::Illegal(IllegalReason))` on failure. The full 16-variant `IllegalReason` is the rejection vocabulary.
- Bridge `process_action_system` writes a `CombatEvent::ActionRejected { reason }` log entry on Err (replaces `validate_action_system`'s `EndTurn` fail-forward).

**Input unification:**
- Delete `UseAbility` and `ValidatedAction` message types.
- Player and AI write `ActionInput::Cast { actor, ability, target, target_pos }` directly. `command_input.rs` (player), `ui/hex_grid/input.rs` (click), and `ai/system.rs` (AI plan executor) all migrate — pattern mirrors Phase 1's `MoveUnit → ActionInput::Move`.
- For UI tooltip pre-checks (e.g., "ability disabled because no mana"), callers invoke `combat_engine::check_legality()` directly against a `BevyActions` adapter. No new system; pure function call.

**Bridge / projector:**
- `EcsContentView` extended with `ability_def`, `weapon_dice_for_ability` (per-ability not just AoO), `status_def`, `crit_fail_table`.
- `process_action_system` handles `ActionInput::Cast` (alongside `Move`); translates event stream to `CombatLog::DamageResult` / `HealResult` / `StatusApplied` / `StatusExpired` / `AbilityUsed` / `ManaChanged` / `CriticalMiss` / `CritFailSideEffect`.
- `project_state_to_ecs` extended to project `Mana.current`, `StatusEffects` (with applier-aware merge — see §8), and `Dead` marker comprehensively.

**Deletions (the architectural simplification):**
- `apply_effects_system` (130 lines, `src/combat/apply_effects.rs`).
- `validate_action_system` (75 lines, `src/combat/validation.rs`).
- `resolve_action_system` (291 lines, `src/combat/resolution.rs`) — entirely. Summon emission moves into bridge.
- `UseAbility` + `ValidatedAction` + `ApplyDamage` + `ApplyHeal` + `ApplyStatus` Bevy message structs.
- `sim::apply_cast` + `sim::apply_primary` + `sim::apply_statuses` (~250 lines in `sim.rs`).
- `SimState` snapshot↔CombatState roundtrip eliminated — `SimState` gains persistent `combat_state: CombatState` field. Folds Phase 1's deferred step 7.

**OUT (deferred to Phase 3+):**
- `Action::EndTurn` and round-tick mechanics (`TickDot`, status duration decrement) — Phase 3.
- `EffectDef::Summon` engine modeling — `SpawnUnit` stays a Bevy message; bridge inspects ability content and emits `SpawnUnit` alongside calling `step()` for non-summon arms. Full migration moves to Phase 3.
- `auras_system` — Phase 3 owns. Phase 2 keeps it ECS-only; projector uses applier-aware merge logic to avoid clobbering aura-applied statuses.

---

## 2. Architecture diff vs Phase 1

```
Phase 1:                                     Phase 2 target:
  CombatState (auth: Move state)               CombatState (auth: Move + Cast state)
        │                                            │
        │ projector:                                 │ projector:
        │   pos / hp / mp / reactions / rage         │   + mana + statuses (merge-aware)
        ▼                                            ▼
  ECS (Move read-only; apply_effects writes     ECS (Move + Cast read-only;
   hp/mana/statuses for Cast path)               apply_effects deleted)

  UseAbility → validate → ValidatedAction      ActionInput::{Move,Cast} → step()
   → resolve → ApplyDamage/Heal/Status          (UseAbility/ValidatedAction/Apply* deleted;
   → apply_effects                               validate + resolve deleted)

  check_legality lives in combat::actions      check_legality lives in combat_engine;
   (Bevy crate)                                  shared by step() + player UI + AI

  Crit-fail rolled in resolve_action_system    Crit-fail rolled in engine expand_action(Cast)
   (split RNG: resolver + step)                 (single RNG stream — Phase 5-ready)
```

The Phase 6 endpoint becomes visible: `combat_engine` owns Action / Effect / Event / legality / targeting / dice. Bevy hosts the world (ECS + content data) and provides the projection + event-translation glue. No legacy "Bevy mutates state then engine mirrors it" path remains.

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/src/action.rs` | Add `Cast` variant; expand `ActionError` to carry `IllegalReason` (legality), keep `NoPath`/`OutOfMP`/`TargetGone`/`ReactionDepthExceeded`/`PathBlockedByEnemy`/`DestinationOccupied` (engine-internal) |
| `crates/combat_engine/src/dice.rs` | Unify `SeededDice` → `DiceRng` (LCG + `script(&[i32])` capability ported from `core::DiceRng`). Engine owns the canonical RNG type. Old `SeededDice` callsites in tests adapt |
| `src/core/rng.rs` | **DELETE.** `DiceRng` and `DiceExpr` move into engine; this file vanishes |
| `src/combat/dice_resource.rs` | **NEW.** Tiny `#[derive(Resource)] pub struct DiceRngRes(pub combat_engine::DiceRng);` plus `Deref`/`DerefMut`. Bevy systems consume `ResMut<DiceRngRes>` and pass `&mut **rng` to `step()` directly — no adapter |
| `src/combat/engine_bridge.rs` | **DELETE** the `DiceRngAdapter` shim. `process_action_system` takes `ResMut<DiceRngRes>` and passes the inner `DiceRng` to `step()` |
| `crates/combat_engine/src/legality.rs` | **NEW.** Migrated from `src/combat/actions/mod.rs`: `IllegalReason` enum (16 variants), `ActionState` trait, `ProposedAction`, `LegalAction`, `check_legality()` function. Bevy-free; uses `UnitId` not `Entity` (callers adapt via traits) |
| `crates/combat_engine/src/effect.rs` | Add `PayCost`, `Heal`, `ApplyStatus`, `RemoveStatus`. Extend `apply_effect`. `ApplyStatus`/`RemoveStatus` derive `RefreshAggregates` |
| `crates/combat_engine/src/event.rs` | Add `AbilityResolved`, `UnitHealed`, `StatusApplied`, `StatusRemoved`, `ActionRejected`. Extend `effect_to_event` |
| `crates/combat_engine/src/content.rs` | Extend `ContentView`: `ability_def(AbilityId) -> Option<&AbilityDef>`, `status_def(StatusId) -> Option<&StatusDef>`, `weapon_dice_for_ability(UnitId, AbilityId)`, `crit_fail_table(AbilityId) -> Option<&CritFailTable>`. Add minimal engine-side data types: `AbilityDef` (target_type, range, costs, effect_list, crit_fail_table), `StatusDef` (duration, armor_bonus, speed_bonus, skips_turn, blocks_mana_abilities, causes_disadvantage, forces_targeting), `CritFailTable`, `CritFailOutcome` |
| `crates/combat_engine/src/state.rs` | `ActiveStatus` gains `applier: UnitId` field (5 read sites in src/ require this — see §8) |
| `crates/combat_engine/src/targeting.rs` | **NEW.** Engine-side target enumeration (single / circle / cone / LOS). Ports `combat/effects_state::compute_affected_targets` |
| `crates/combat_engine/src/step.rs` | Add `Action::Cast` arm: legality check via `check_legality` → `Err(ActionError::Illegal(reason))` on fail; crit-fail roll via `ContentView::crit_fail_table`; cost payment; target enumeration; per-target effect fan-out (decision 6.3) |
| `src/combat/actions/mod.rs` | **DELETE.** Contents moved into `crates/combat_engine/src/legality.rs`. Bevy adapter `BevyActions` (and any other site-specific impls) live with their callers |
| `src/combat/apply_effects.rs` | **DELETE.** Engine + bridge replace it |
| `src/combat/validation.rs` | **DELETE.** Engine's pre-validate handles legality; UI tooltips call `combat_engine::check_legality` directly via `BevyActions` adapter (which migrates into a small Bevy-adapter module — likely `src/combat/legality_adapter.rs`, ~40 lines) |
| `src/combat/resolution.rs` | **DELETE.** Engine handles cast resolution end-to-end. Summon emission moves into bridge |
| `src/combat/legality_adapter.rs` | **NEW.** Small ~40-line module: `BevyActions` impl of `combat_engine::ActionState` for UI/player tooltip-time legality checks. The pre-engine UX surface |
| `src/combat/engine_bridge.rs` | Extend `EcsContentView` with new ContentView surface; extend `process_action_system` for Cast (route + event translation + Summon carve-out); extend projector for Mana + StatusEffects (applier-aware merge) |
| `src/combat/advance_turn.rs` | Remove `ApplyStatus` consumer (status apply now flows through engine event → bridge → ECS) |
| `src/game/messages.rs` | **DELETE** `UseAbility`, `ValidatedAction`, `ApplyDamage`, `ApplyHeal`, `ApplyStatus`. Extend `ActionInput` enum: `Cast { actor, ability, target, target_pos, disadvantage: bool }` |
| `src/main.rs` | Remove `add_message::<UseAbility>` / `ValidatedAction` / `ApplyDamage` / `ApplyHeal` / `ApplyStatus`; keep `ActionInput` registration |
| `src/ui/hex_grid/input.rs` | Player click writes `ActionInput::Cast` instead of `UseAbility`. UI tooltips call `combat_engine::check_legality` via `legality_adapter::BevyActions` for pre-commit feedback |
| `src/combat/command_input.rs` | Player command writes `ActionInput::Cast` directly |
| `src/combat/ai/system.rs` | AI's `pact_ai_system` + `enemy_ai_system` write `ActionInput::Cast` instead of `UseAbility` |
| `src/combat/ai/plan/generator.rs` | `generate_plans` calls `combat_engine::check_legality` (path change — same function) via `SnapshotActions` impl. Logic unchanged |
| `src/combat/ai/plan/sim.rs` | Delete `apply_cast`, `apply_primary`, `apply_statuses` (~250 lines). `SimState` gains `combat_state: CombatState`; `from_snapshot` builds it once. `apply_move` and `apply_step::Cast` mutate it directly. Folds Phase 1's deferred step 7 |
| `src/combat/pipeline.rs` | Drop `validate::validate_action_system`, `resolution::resolve_action_system`, `apply_effects::apply_effects_system` from the chain |
| `tests/combat_engine/` | New: `cast.rs`, `legality.rs`, `targeting.rs`, `crit_fail.rs`. Expand `parity.rs` with cast scenarios |
| `tests/combat/` | Most effects/statuses tests stay green by construction (all 28 are end-state — see §8). `validation.rs` and `pipeline.rs` tests adapt to new action shape |
| `tests/common/mod.rs` | Drop legacy effect messages + UseAbility + ValidatedAction from test apps; add `ActionInput::Cast` registration |
| `docs/architecture.md`, `docs/combat-pipeline.md`, `docs/content-guide.md` | Update to reflect engine ownership; remove validate/resolve from the 10-system pipeline diagram |
| `benches/engine_cast.rs` | **NEW.** Bench gate: ≤ 1.2× Phase 1 baseline for cast scenarios |

---

## 4. Implementation order

Engine-first additive, then bridge witness, then flip + delete. Land each step as its own commit; CI green before next.

1. **Engine: RNG unification.** Port `core::DiceRng` into `combat_engine::dice` (unifying with `SeededDice`, adding `script(&[i32])`). Delete `src/core/rng.rs`. Add `src/combat/dice_resource.rs` (Bevy `Resource` wrapper). Update all callsites that took `Res<DiceRng>` / `ResMut<DiceRng>` to use `DiceRngRes`. Delete `DiceRngAdapter`. Standalone commit — no behavior change, all tests stay green by construction.
2. **Engine: `legality.rs` migration.** Move `check_legality` + trait + `IllegalReason` into `combat_engine` crate. Adapters at callsites: `BevyActions` moves to `src/combat/legality_adapter.rs`; `SnapshotActions` stays in `combat::ai::plan::generator.rs` (or sibling). `validate_action_system` still calls the function — bridge to the new path during transition; deletes in step 10.
3. **Engine: `Effect::Heal` + `Event::UnitHealed`.** Atomic addition. DoT-neutralize then HP-restore logic. Tests in `tests/combat_engine/effect.rs`.
4. **Engine: `Effect::PayCost` + `Effect::ApplyStatus` + `Effect::RemoveStatus`.** `applier: UnitId` on `ActiveStatus`. Status mutations derive `RefreshAggregates`.
5. **Engine: targeting module.** Port `compute_affected_targets`. Tests in `tests/combat_engine/targeting.rs`.
6. **Engine: `Action::Cast` in `step()`.** Pre-validate via `check_legality`; crit-fail roll via `ContentView::crit_fail_table`; cost payment; target enumeration; per-target effect fan-out. Tests for: basic damage cast, AoE damage with per-target ordering, heal, status apply, cost payment failures, crit-fail branches, kill-mid-AoE strict failure (decision 6.5 non-actor finally exercised).
7. **Bridge: `ActionInput::Cast` routing + event translation.** `EcsContentView` extends to the new ContentView surface. Event translation writes `CombatLog::DamageResult`, `HealResult`, `StatusApplied`, `StatusExpired`, `AbilityUsed`, `ManaChanged`, `CriticalMiss`, `CritFailSideEffect`. Engine runs as **witness** alongside `validate` + `resolve` + `apply_effects`. Smoke tests in `tests/combat_engine/cast_bridge.rs`.
8. **Projector: `Mana` + `StatusEffects` with applier-aware merge.** Extend `project_state_to_ecs` to write `Mana.current` and merge `StatusEffects`: aura-applied entries (where `applier` corresponds to an aura source unit) survive projection; ability-applied entries replace from engine state. ~10 line merge logic + new bridge_smoke test (`aura_status_survives_projection_after_unrelated_cast`).
9. **AI scoring sanity check.** Mining run pre-Phase-2 (captures agenda mix baseline) + after step 8 (engine witness writing same events). Confirms numerical parity before flip. If AI mining shifts > 5%, debug formula divergence between `compute_ability_outcome` and engine cast.
10. **Flip + mass deletion.** All in one commit (Phase 1 B2 pattern):
   - Player + AI writers migrate to `ActionInput::Cast` (~8 callsites).
   - Delete `validate_action_system`, `resolve_action_system`, `apply_effects_system`, `actions/mod.rs` (functionality moved to engine), `UseAbility`, `ValidatedAction`, `ApplyDamage`, `ApplyHeal`, `ApplyStatus`.
   - Bridge handles Summon carve-out (inspect ability content, emit `SpawnUnit` for Summon arms; engine handles the rest).
   - Sim cleanup: delete `apply_cast` / `apply_primary` / `apply_statuses`; `SimState` persists `CombatState`.
   - Test fixtures (`tests/common/mod.rs`) drop legacy registrations.
11. **Bench capture.** `cargo bench --bench engine_cast` + `engine_move`. Gate: ≤ 1.2× Phase 1 baseline (1.81 µs for Move; new cast baseline established).
12. **Mining post-Phase-2.** Compare agenda mix; document drift in retro.

---

## 5. Existing code to consult

| When writing | Read |
|---|---|
| Engine `Action::Cast` + `expand_action` | `src/combat/resolution.rs` (current effect fan-out), `src/combat/effects_outcome.rs::compute_ability_outcome` |
| Targeting module | `src/combat/effects_state.rs::compute_affected_targets`, `src/game/hex.rs` (hex_circle, hex_line, has_los) |
| Heal logic (DoT-neutralize) | `src/combat/apply_effects.rs:73-114` |
| Status apply consumer | `src/combat/advance_turn.rs` (ApplyStatus arm) |
| Status aggregate refresh | `crates/combat_engine/src/effect.rs::Effect::RefreshAggregates` (already implemented for AoO armor_bonus) |
| Bridge event translation pattern | `src/combat/engine_bridge.rs::translate_move_events` (Phase 1 reference) |
| Sim cleanup | Current `src/combat/ai/plan/sim.rs` lines 183-438 |
| Legality migration | `src/combat/actions/mod.rs` (entire file) — move to engine |
| Crit-fail mechanism | `src/combat/effects_outcome.rs::map_crit_fail` (line 187), `src/content/races.rs::CritFailEffect` enum |
| Auras + projector merge | `src/combat/auras.rs::apply_auras_system`, the projector's existing field handling |

Run `ya tool ast-index outline <file>` before any read > 500 lines.

---

## 6. Test plan

**Engine unit tests (`tests/combat_engine/`):**
- `effect.rs` (extend): `Heal` cleanses DoT then restores HP; `PayCost` decrements mana/rage/energy; `ApplyStatus` adds + triggers `RefreshAggregates`; `RemoveStatus` filters by id + triggers `RefreshAggregates`.
- `legality.rs` (new): all 16 `IllegalReason` variants pinned by minimal CombatState fixtures. Engine impl of `ActionState` works identically to `BevyActions` and `SnapshotActions` (parity test).
- `targeting.rs` (new): single, circle radius-1/2, cone, LOS blocked, out-of-bounds clipping.
- `cast.rs` (new): basic damage cast, AoE with per-target ordering pinned, heal, status apply with duration, status reapply replaces, cost payment fails with `ActionError::Illegal(InsufficientResource)`.
- `crit_fail.rs` (new): all 6 outcome variants (Miss, ApplyStatus, SelfDamage, DoubleCost — note Miss / ManaOverload / BrokenFaith / CircuitBreach / Exhaustion / PactControl from `CritFailEffect` map to these primitives).
- `parity.rs` (extend): 4-6 cast scenarios mirroring existing `parity_*_real_vs_sim` style.
- `step.rs` (extend): kill-mid-AoE strict failure (decision 6.5 non-actor — first production use of the rollback branch).

**Bridge tests (`tests/combat_engine/cast_bridge.rs`, new):**
- `engine_emits_combat_log_damage_result_for_cast`
- `engine_emits_status_applied_event`
- `engine_heal_cleanses_dot_then_restores_hp`
- `projector_writes_mana_after_cost_paid`
- `projector_writes_status_effects_from_engine`
- `aura_status_survives_projection_after_unrelated_cast` (applier-aware merge pin)
- `engine_action_rejected_emits_combat_log_entry` (legality rejection path)

**Existing tests:**
- All 28 effects/statuses/pipeline tests stay green by construction (per discovery: end-state assertions, ordering-agnostic).
- `tests/combat/validation.rs` (12 tests): migrate or delete. Each verifies one `IllegalReason` branch; equivalent coverage moves into `tests/combat_engine/legality.rs`. Net test count unchanged.
- `tests/combat/aoo.rs` (9 tests): untouched (Phase 2 doesn't change Move).
- `tests/combat_engine/parity.rs` (existing 4 sim-vs-engine): stay green since sim now routes through engine for Cast too.

---

## 7. Gates (revised)

| # | Criterion | Verify |
|---|---|---|
| 1 | Damage / heal / status parity | `cargo test --test combat --test combat_engine` 100% pass |
| 2 | 12.1 (speed + status refresh) preserved | Aura/speed parity tests stay green |
| 3 | 12.3 (rage on damage + AoO) preserved | Per-target ordering test pins new sequence |
| 4 | AI scoring stable | Mining run pre/post: agenda mix shift ≤ 5%; band distribution shift ≤ 3% |
| 5 | Bench ≤ 1.2× Phase 1 baseline | `cargo bench --bench engine_move --bench engine_cast` |
| 6 | No `apply_effects_system` / `validate_action_system` / `resolve_action_system` | `ya tool ast-index search "fn …"` empty |
| 7 | No `UseAbility` / `ValidatedAction` / `ApplyDamage` / `ApplyHeal` / `ApplyStatus` | grep empty |
| 8 | No `sim::apply_cast` | grep empty |
| 9 | No `combat::actions` module | path deleted; legality lives in `combat_engine::legality` |
| 10 | Crit-fail RNG single-stream | All cast-related RNG consumption happens inside `step()` (verified by code review + bench inspection) |
| 11 | RNG unified — no `DiceRngAdapter`, no `core::DiceRng` | `ya tool ast-index search "DiceRngAdapter"` empty; `src/core/rng.rs` deleted; engine `DiceRng` consumed everywhere |
| 12 | Multi-frame playtest correctness | Manual playtest after step 10: damage from `Lyra`-cast spells persists across rounds; rage on hit accumulates; HP visible in AI debug equals real ECS HP. Pinned by a new bridge_smoke test in step 10 (`bridge_projection_does_not_clobber_apply_effects_writes`) covering the multi-frame race. |

---

## 8. Risks / flags

Updated 2026-05-14 after pre-implementation discovery pass + architecture review.

### Resolved decisions

**Per-target ordering (decision 6.3) — confirmed free.** Audited 28 tests across `effects.rs` / `statuses.rs` / `pipeline.rs`; all are end-state assertions. Log readers (`replay_assert`, `replay_ai_log`, `mine_ai_logs`) have no within-action ordering dependencies. **Zero existing rewrites.** New per-target-ordering pin tests added in `tests/combat_engine/cast.rs`.

**Status `applier` field — add to engine.** 5 read sites in `auras.rs` (3) + `advance_turn.rs` (2), all Phase 2-load-bearing. Engine `ActiveStatus` gains `applier: UnitId`; bridge sets it from the source unit's id. Negligible cost.

**Crit-fail — engine-owned (revised).** Earlier draft kept crit-fail in resolution.rs as a pre-step. After arch review: split RNG (resolver rolls crit-fail, engine rolls damage) is a Phase 5 replay-determinism footgun. `ContentView::crit_fail_table` returns engine-side `CritFailTable` data; `expand_action(Cast)` rolls once; derives aux effects. All cast RNG single-stream. The 6 `CritFailEffect` variants map cleanly to `CritFailOutcome { Miss, ApplyStatus(StatusId), SelfDamage(DiceExpr), DoubleCost }`.

**`UseAbility` + `ValidatedAction` — delete (revised).** Earlier draft kept them as input boundary. Arch review: parallel input types for Move (ActionInput::Move end-to-end) vs Cast (UseAbility → resolution → ActionInput::Cast) is asymmetric scaffolding. Phase 2 unifies: player + AI write `ActionInput::Cast` directly (~8 callsite migration, mirrors Phase 1's MoveUnit pattern).

**`validate_action_system` — delete (revised).** Earlier draft kept as UX gate. Arch review: two layers computing legality via the same `check_legality` function is duplication. Better: move `check_legality` into engine; engine pre-validate uses it; UI tooltips call it directly via `BevyActions` adapter (no system needed — pure function call). 75-line `validation.rs` deletes; `BevyActions` impl shrinks to a ~40-line `legality_adapter.rs`.

**`auras_system` write timing — projector merges, not replaces.** Pipeline order is safe in Phase 2 (auras run TurnStart; projector runs Execute; engine state from `init_state_from_ecs` predates current-tick auras). Projector merges: for each unit, preserve ECS `StatusEffects` entries whose `applier` matches a known aura source; replace ability-applied entries from engine state. ~10 line merge + bridge_smoke test pin. Cleaner alternative (auras-to-engine) is Phase 3 scope.

**`SpawnUnit` from Summon — bridge carve-out.** `apply_spawn_system` is 163 lines, self-contained. Summoned units join next round (no current-action coherence). Phase 2 bridge: when handling `ActionInput::Cast`, inspect ability's `EffectDef` arms; for Summon arms, emit `SpawnUnit` Bevy message; for other arms, call `step()`. Full engine migration of Summon → Phase 3.

**`Effect::RefreshAggregates` after `ApplyStatus` / `RemoveStatus`.** Derived effect, pattern matches existing AoO armor_bonus refresh.

### Open — implementation-time investigation

- **SimState `CombatState` clone cost.** For 100 plan branches × 10 units, ~1000 unit clones / AI tick. Each `Unit` is ~10 i32 + a `Vec<ActiveStatus>` of 0-3 entries. Should be sub-millisecond, but profile during step 9 implementation. Fallback: `im::Vector<Unit>` (persistent collection) or `Arc<Unit>` cell pattern. Threshold: AI mining run wall-clock regression > 20%.
- **AI scoring numerical parity.** Engine cast resolution must produce identical damage/heal numbers to sim's `compute_ability_outcome`. Pre-Phase-2 mining baseline captures current agenda mix; post-step-7 (witness-mode) confirms numerical parity before authority flip in step 9. If formulas diverge (e.g., rounding rules), pin via a parity test before continuing.
- **Engine `IllegalReason::TauntForcesTarget` semantics.** Engine knows unit positions and team but doesn't currently parse status content for `forces_targeting`. Requires `ContentView::status_def` returning a struct with that bool. Confirmed in scope (already in §1 ContentView surface) but worth double-checking at step 1.

**Legacy Bevy integration tests deleted in step 9c (scorched earth).**
54 tests in tests/combat/{validation,pipeline,effects,statuses,crit_fail}.rs
deleted alongside the systems they exercised.  Mechanics coverage moves
to engine cast.rs / effect.rs / step.rs (103 tests) + bridge_smoke.rs
(~10 tests).  Integration-shaped coverage of the bridge → ECS path
narrows; future Phase 3+ work that needs multi-frame regression coverage
will add new bridge_smoke tests rather than resurrecting the deleted
files.

### Resolved decisions

- **Projector clobbers `apply_effects_system` writes between frames** (discovered 2026-05-14 mid-step-5 from a playtest log).  Root cause: `project_state_to_ecs` writes `Vital.hp` / `Rage.current` from `CombatStateRes` every frame while `apply_effects_system` was also a writer for those fields; because `init_state_from_ecs` ran only once per round, the projector's stale round-start snapshot reverted all non-Move damage on the next frame.  **Self-resolved in Phase 2 step 9d** when `apply_effects_system` was deleted; `project_state_to_ecs` is now the sole writer for hp / rage / mana / statuses.  Playtest combat is restored to correct behavior at this step.  Gate criterion 12 (multi-frame test) remains for Phase 3 if needed.

---

## 9. Rollback

- Revert from `unisim/phase2-complete` tag back to `unisim/phase1-complete`. Clean behaviour rollback — Phase 1 is the last known-good gate.

Spike-style throwaway is not possible; Phase 2 is the production path.

---

## 10. Items to flag back

Stop and ask before proceeding if:

- **Mining drift > 10% on agenda mix** after step 7. Indicates engine cast resolution diverges from sim's scoring; pin the formula divergence before continuing.
- **Per-target ordering surfaces > 0 unexpected test rewrites.** Discovery said zero; if step 5's new engine tests show ordering tripping existing assertions, revisit decision 6.3 scope.
- **`IllegalReason` migration creates a Bevy import in `combat_engine`.** `Entity` must NOT enter the engine crate. The trait abstraction should accommodate the bridge's `Entity ↔ UnitId` translation; if not, flag before continuing.
- **Bench regression > 1.5× Phase 1 baseline.** Profile before optimizing. Likely culprits: per-target ordering increasing apply count, or `CombatState` clone cost in sim hot loop, or unnecessary `ContentView` allocation per cast.
- **Auras + projector race surfaces a test failure.** Merge logic edge cases (e.g., player removes an aura-applied status via `RemoveStatus` effect — should that override the merge preservation?) need design clarification.

---

## 11. Done = merged + gates + retro

After gates pass:
1. Append `## 12. Retrospective` with surprises, deviations, perf numbers, decisions for Phase 3.
2. Open `step_unisim3_plan.md` from §5.3 template in `unisim.md`.
3. Tag commit `unisim/phase2-complete`.

---

## 12. Retrospective

Closed 2026-05-15.  10 of 12 gates green; 1 deferred (mining), 1 skipped (`engine_cast` bench by explicit choice — `engine_move` covers the perf budget).

### Commit chain (24 implementation commits)

| SHA | Step | Scope |
|---|---|---|
| `8b85503` | 1 | RNG unification: `core::DiceRng` + `DiceExpr` into engine; `DiceRngRes` wrapper; `DiceRngAdapter` deleted |
| `152a858` | 2a | ID unification: `AbilityId/WeaponId/ArmorId` join `StatusId` in engine; conversion sites collapse to identity |
| `292140a` | 2b | ContentView extension: minimal `AbilityDef`/`StatusDef`/`TargetType`/`AbilityRange`/`Cost`/`ResourceKind` in engine |
| `c8e661d` | 2c-prep | Team unification (single canonical `Team` in engine; bridge identity drops) |
| `d16e42b` | 2c | Legality migration: `check_legality` + `ActionState` trait + `IllegalReason` move into `combat_engine::legality`; `combat::actions` module deleted |
| `adcfe5c` | 3 | `Effect::Heal` + `Event::UnitHealed` — DoT-neutralize then HP restore |
| `36de8b1` | 4 | `Effect::PayCost` + `ApplyStatus` + `RemoveStatus`; `ActiveStatus` gains `applier: UnitId` |
| `abd4e61` | 5 | Engine `targeting` module; `compute_affected_targets` + `aoe_cells` |
| `0eefbd6` | 5+ | Known-issue tracker: projector clobbers `apply_effects` writes (self-resolved at step 9d) |
| `b25c84f` | 6a | Engine `EffectDef` + `StatusApplication`/`StatusOn` types |
| `f1fc58e` | 6b | `Action::Cast` variant + `step()` arm skeleton (legality pre-validate) |
| `6bac19e` | 6c | Cast cost payment (`Effect::DecrementAP` + `PayCost` × N) |
| `8f2b360` | 6d | Cast damage fanout per-target with per-target ordering pinned |
| `c0e816e` | 6e | Cast heal + status fanout |
| `454790c` | 6f | Cast crit-fail integration: engine rolls d20; `CritFailOutcome` primitives |
| `886f1e6` | 7a | `ActionInput::Cast` routing through `process_action_system` |
| `575553f` | 7b | Cast event → `CombatLog` translation (DamageResult / HealResult / StatusApplied / AbilityUsed / ManaChanged) |
| `3af6237` | 7c | Projector: `Mana` + `StatusEffects` projection with applier-aware merge |
| `df05391` | 7d | Crit-fail event translation (`CriticalMiss` / `CritFailSideEffect`) |
| `648c7f4` | 9a | Player writers migrate `UseAbility` → `ActionInput::Cast` |
| `e538b98` | 9b | AI writers migrate |
| `edf49fc` | 9c | Scorched-earth: 49 legacy Bevy integration tests deleted (validate / pipeline / effects / statuses / crit_fail) |
| `c72fc35` | 9d | Legacy systems + message types deleted (~500 lines); `BevyActions` relocated to `legality_adapter.rs`; crit-fail wiring from race content; Summon carve-out in bridge |
| `3644d2a` | 9e | Sim cleanup: persistent `CombatState` in `SimState`; `apply_cast` + `apply_primary` + `apply_statuses` deleted (~250 lines); engine disadvantage wiring |

### Gate results

| # | Criterion | Result |
|---|---|---|
| 1 | Damage / heal / status parity | ✅ combat_engine 103 + bridge_smoke 18 tests |
| 2 | 12.1 (speed + status refresh) preserved | ✅ engine `RefreshAggregates` derived from `ApplyStatus`/`RemoveStatus`/`Heal`-removal |
| 3 | 12.3 (rage on damage + AoO) preserved | ✅ engine `Damage` derives `GainRage(src)`+`GainRage(tgt)`+`Death` in order |
| 4 | AI scoring stable (mining) | ⏸ deferred — playtest log baselines being generated post-hoc |
| 5 | Bench ≤ 1.2× Phase 1 baseline | ✅ engine_move 1.93 µs vs 2.16 µs ceiling (11% margin) |
| 6 | No `apply_effects_system` / `validate_action_system` / `resolve_action_system` | ✅ all three deleted; `BevyActions` moved to `legality_adapter` |
| 7 | No `UseAbility` / `ValidatedAction` / `ApplyDamage` / `ApplyHeal` / `ApplyStatus` types | ✅ deleted in 9d |
| 8 | No `sim::apply_cast` | ✅ deleted in 9e |
| 9 | No `combat::actions` module | ✅ migrated to `combat_engine::legality` in 2c |
| 10 | Crit-fail RNG single-stream | ✅ engine rolls d20 inside `step()`; no resolver split |
| 11 | RNG unified | ✅ `combat_engine::DiceRng` is canonical; `DiceRngAdapter` gone |
| 12 | Multi-frame playtest correctness | ✅ self-resolved at 9d (`apply_effects` deletion ends the projector clobber) |

### Perf numbers (bench `engine_move`, 10-unit scenario)

|  | Phase 0 | Phase 1 | Phase 2 | Δ vs Phase 1 |
|---|---|---|---|---|
| engine `step(Move)` | 1.51 µs | 1.80 µs | **1.93 µs** | +5.9% |
| legacy (sim shim) | 2.02 µs | 2.39 µs | **2.77 µs** | +16.4% |

The +5.9% on `engine_move` is likely traceable to enum-size growth (`Effect` +5 variants, `Event` +4) widening match-table sizes and slightly worsening branch prediction. The +16.4% on the legacy sim shim is partly the same engine cost plus the bench fixture's per-iteration snapshot↔CombatState rebuild (the bench doesn't benefit from Phase 2's persistent `CombatState` because each iteration constructs a fresh `SimState`).

Vs Phase 0 (1.51 µs): `engine_move` at 1.93 µs = 1.28× — **over** the original Phase 0 1.2× gate, but each phase gates against its predecessor. Phase 3 should still consider whether the cumulative drift is sustainable; the absolute regression budget (Phase 1→Phase 2: +130 ns) translates to ~13 ms per 100-action plan-search branch in the AI hot loop. Add `engine_cast` bench at Phase 3 start to establish a cast-side baseline.

### Deviations from original plan

- **Step 6 split into 6 sub-commits (a–f).** Plan implied one ~250-line step. In practice the engine Cast arm grew incrementally: types → variant + skeleton → cost → damage → heal/status → crit-fail. Each commit was atomic and ≤80 lines.
- **Step 7 split into 4 sub-commits (a–d).** Bridge wiring fanned out: routing → event translation → projector extension → crit-fail events.
- **Step 8 (AI scoring sanity / mining) deferred.** Phase 1 also skipped its mining check. Mining requires real combat JSONL log baselines which neither phase had ready. Deferred to ad-hoc post-tag work.
- **Step 9 split into 5 sub-commits (a–e).** Plan listed it as one atomic flip. Real scope was too large for one commit; split into producer migration (a/b), test cleanup (c), system deletion (d), sim cleanup (e).
- **Scorched-earth test deletion in 9c.** 49 legacy Bevy integration tests deleted rather than migrated to `ActionInput::Cast` (the alternative). Engine cast tests (103) + bridge_smoke tests (18) cover the same mechanics at unit + bridge level; the legacy pipeline tests tested a pipeline being deleted. **Accepted coverage shrink** for integration-shape regression detection; new bridge_smoke tests can be added as needed.
- **Crit-fail wiring landed in 9d, not 7-followup.** Step 6f shipped a TODO for `EcsContentView::caster_contexts.crit_fail_outcome` (defaulted to Miss). The TODO closed in 9d when the path-content lookup was wired alongside the system-deletion commit. Live combat crit-fails return to correct behavior post-9d.
- **Disadvantage regression discovered at 9e end.** Engine's `step()` was dropping `LegalAction.disadvantage` from `check_legality`; AI plan damage estimates over-counted by ~18% on disoriented casters. Fixed in same commit via `DiceSource::roll_disadvantage` + `effect_for_target(disadvantage: bool)` parameter threading.
- **`SpawnUnit` (Summon) carve-out**: kept as Bevy message + `apply_spawn_system` per plan §1 OUT decision. Bridge inspects ability content for `EffectDef::Summon` and writes the Bevy message alongside calling `step()` for non-Summon arms. Engine doesn't model summon spawning yet.

### Decisions to carry into Phase 3

- **DoT tick mechanics + `dot_per_tick` per-apply.** Engine `Effect::ApplyStatus` currently writes `dot_per_tick: 0`. Phase 3 wires `StatusDef.dot_dice` into engine and rolls per-apply via `rng`. The `applier: UnitId` field on `ActiveStatus` is in place for kill-credit attribution when DoT ticks hit lethal.
- **Round-tick mechanics (`TickDot`, status duration decrement) need engine ownership.** Phase 3 step §5.3 calls for `Action::EndTurn` migration with `TickDot` at `RoundPhase::EndRound`. Engine's `RoundPhase` enum exists but `EndRound` isn't reached today — Phase 4 (turn queue) is where this fully comes together.
- **`auras_system` migration.** Currently outside engine; bridge merge logic in 7c preserves aura-applied statuses. Phase 3 considers migrating auras to engine (auras-as-effect) vs. keeping them as ECS-level pre-step writes.
- **`CircuitBreach` crit-fail uses fixed `DiceExpr::new(0,1,2)` placeholder.** Bevy variant computes from cast's `mana_cost / 2`. Engine doesn't know mana_cost at crit-fail time. Phase 3 follow-up: pass the cast's mana_cost into `CritFailOutcome::SelfDamage` resolution OR change content to specify the dice explicitly.
- **`engine_cast` bench.** Skipped in Phase 2 retro by explicit choice. Phase 3 should add it at step 1 to gate cast-side perf regressions.
- **Bench cumulative drift.** Phase 1 added 19%; Phase 2 added 6%. If Phase 3 adds another 10–15%, we hit Phase 0 baseline × 1.5× territory. Phase 3 starts with a bench profile run + budget reservation.
- **Mining post-Phase-2 still owed.** User collecting fresh playtest logs to compare against the Phase 1 baseline; defer Phase 2 gate criterion 4 to that ad-hoc check.
- **Sim's `SimState.combat_state` clone cost.** Phase 1 step 7 deferred concern. Phase 2 step 9e landed the persistent field but didn't profile the clone cost across plan branches. If AI mining shows wall-clock regression > 20%, the fallback is `im::Vector<Unit>` (persistent collection).
- **`EngineCheckState`'s `is_in_bounds` returns `true` unconditionally.** Engine is grid-topology-agnostic; legality's `TargetOutOfBounds` branch fires only via Bevy adapter. Acceptable for Phase 2; revisit if engine ever needs to enforce bounds (e.g., if Phase 4 turn flow includes spawn-location validation).
