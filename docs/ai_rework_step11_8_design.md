# Step 11.8 Calibration Design

> **Status:** Draft for review (post H3a/H3b/H3c mining + HardRescue fix).
>
> **Post-implementation findings:** see `docs/ai_rework_step11_8_findings.md`.
> Gate 2 passed strongly (Forced fallback 45.9% → 14.3%, −68.8% relative).
> Gate 1 partial: ProtectSelf leverage and feasibility alive; Reposition/AOE
> leverage flat (corpus-bound + minor formula edge — not a code bug; deferred
> to post-11.9 calibration slice).

## Source data

- 11.7 mining sections (H1c / H2 / H3a / H3b / H3c) on `logs/*.jsonl` after 11.7 reject-reason instrumentation, second corpus collected 2026-04-30.
- Sample size: 211 ticks (192 with band attribution, 75 unattributed fallback).
- HardRescue/FocusTarget `target=None` bug fixed in commit `41579a5` — H3a row will read 0% on next collection.

> **Caveat — calibration smoke, not statistical proof.** N=211 (75 fallbacks). Per-band buckets have ~5–10pp noise floor. Treat tight numerical bounds in this doc as approximate; if a measurement falls 1–2pp outside a gate threshold, judge by direction (better/worse) rather than literal pass/fail.

### H1c — current state of considerations axes (pre-11.8)

```
urgency              mean=0.376  p10=0.000  p50=0.462  p90=0.970  p99=1.000   ← signal
feasibility          mean=1.000  p10=1.000  p50=1.000  p90=1.000  p99=1.000   ← FLAT
leverage             mean=0.000  p10=0.000  p50=0.000  p90=0.000  p99=0.000   ← FLAT
safety               mean=1.000  p10=1.000  p50=1.000  p90=1.000  p99=1.000   ← FLAT (corpus-bound)
role_affinity        mean=0.668  p10=0.300  p50=0.700  p90=1.000  p99=1.000   ← signal
continuation_value   mean=0.151  p10=0.000  p50=0.000  p90=0.491  p99=0.499   ← signal
```

### H3c — fallback cause classification (75 unattributed ticks)

```
Band                  fallback%  ap_mp%  no_tgt%  no_attempt%  only_move%  unreach%  unclass%
ForcedTargeting        45.9%     0.0%     0.0%       64.3%       14.3%     14.3%      7.1%
NormalTactical         56.1%     0.0%     0.0%        2.2%       26.1%     56.5%     15.2%
HardRescue              2.8%     0.0%   100.0%        0.0%        0.0%      0.0%      0.0%
```

Interpretation:
- **NormalTactical fallback is mostly physics** (`target_unreachable` 56.5%, `only_move_plans` 26.1% — both correlate with target out of reach this turn).
- **ForcedTargeting fallback is generator gap** (`no_plan_attempts_target` 64.3% — pool has Cast plans, none target the taunter).
- HardRescue fallback eliminated by `41579a5`.

## 11.7b Finding: `exposure_at_end` source verification

**Implementation:** `compute_exposure_at_end(plan, ctx)` = `ctx.maps.danger.get(plan.final_pos).clamp(0.0, 1.0)` — reads the danger influence map at the plan's final position.

**Verdict:** Implementation is correct (verified by `exposure_at_end_non_zero_when_actor_in_enemy_threat_zone` and `exposure_at_end_zero_in_safe_backline` in `planning::terminal::tests`). The `safety=1.0` constant in H1c is **scenario-bound**: AI in current corpus reliably ends turns in safe tiles thanks to `OvercommitIntoDanger` critic + scenario design. Not a code bug.

## Per-axis decisions

### A. Fallback semantics

**NormalTactical:** keep strict `plan_is_offensive_vs(plan, target)` eligibility. Fallback is accepted — H3c shows it's dominated by `target_unreachable` (physics) and `only_move_plans` (actor approaching target over multiple turns). Rationale: NormalTactical agenda is **guidance**, not obligation. If actor cannot engage the priority target this turn, picking the best move-only plan via legacy pipeline score is the right behavior.

> **Explicit tradeoff — NT approach plans remain unattributed by design.** H3c shows ~26% of NT fallbacks come from `only_move_plans` ticks where the actor moves toward the NT primary target. Under this design these remain unattributed (composed = pipeline `ann.score`). Rationale: NT is soft guidance; we do **not** want to dilute FocusTarget semantics with "moves toward someone" credit in the most common band. If mining shows the attribution gap matters for downstream tools, revisit in v2.

**ForcedTargeting:** add `ApproachTarget` eligibility relaxation — *only* for ForcedTargeting band, *only* when no plan attempts the target offensively. Rationale: taunt is a mechanical pressure mechanic; if AI literally cannot engage the taunter, moving toward it is the next best behavior. H3c shows 64% of Forced fallback is `no_plan_attempts_target` — these tics currently pick a fallback plan with no agenda attribution; with approach eligibility, they get attributed to the FocusTarget item via "plan reduces distance to taunter."

**Approach definition (first version, intentionally simple):**
```
target_pos = snap.unit(item.target).pos    // taunter's start-of-turn position from snapshot

approaches_target(plan, target_pos) =
    plan.final_pos.distance(target_pos) < plan.start_pos.distance(target_pos)
    AND plan contains at least one Move step
    AND plan.viability.passed (already filtered by ViabilityStage)
```

> **Known v1 limitation — geometric, not path distance.** `final_pos.distance(target_pos)` is hex/Euclidean distance, ignoring walls and impassable terrain. A plan that ends "geometrically closer" but on the other side of an obstacle still counts as approach. Path-distance variant (using `pathfinding::distance`) is more semantically faithful but more expensive — deferred to backlog. v1 trades precision for simplicity; mining will reveal if the gap matters.

**Scope discipline — pool-level fallback, not per-plan-level:** ApproachTarget is **not** a global FocusTarget relaxation. It is enabled **per-tick at pool level**, not per-plan:
- only when `agenda.band == ForcedTargeting` (not for NormalTactical / HardRescue / CSP), AND
- only when **no plan in the entire pool** satisfies `plan_is_offensive_vs(plan, taunter)` — i.e., the tick is genuinely "no direct attack possible." If even one offensive plan exists, approach-only plans stay ineligible so they cannot compete with the actual attack.

The two guards (movement step, viability passed) prevent zero-step plans or already-filtered plans from being silently re-eligible. The "Forced-only / pool-level-fallback-only" rule prevents semantic dilution of FocusTarget eligibility — both in other bands AND in Forced ticks where a real attack is available.

**Implementation requires a pool-level pre-pass** (cheap O(N)): at the start of `ItemScoringStage` — which runs **after** `ViabilityStage` per pipeline order — scan the (post-Viability) pool once for `any plan_is_offensive_vs(plan, taunter)`. Cache the boolean. Use it inside the per-plan eligibility loop. ApproachTarget eligibility kicks in only when this pool-flag is `false`.

> **Pool semantics — post-Viability, not pre-Viability.** The pre-pass must observe the pool **after** ViabilityStage has run. Otherwise an unviable offensive plan (which cannot actually be picked) would block ApproachTarget eligibility for the viable approach plans. Placing the pre-pass at the start of `ItemScoringStage` gives this naturally — the stage runs post-Viability per existing pipeline order.

Plan IS eligible under FocusTarget when:
- `plan_is_offensive_vs(plan, target)` (existing filter, primary path), OR
- `agenda.band == ForcedTargeting && pool_has_no_offensive_plan_vs(target) && approaches_target(plan, target_pos)` (pool-level fallback path)

**Future stricter alternative (backlog):** `can_cast_next_turn(plan, target)` — accounts for actor's attack range / cast range / AP-after-plan. Defer until current rule shows weakness.

**Rejected alternatives:**
- Global approach eligibility (NormalTactical + Forced) — would dilute FocusTarget semantics. NormalTactical is guidance, Forced is obligation.
- Eligibility based on minimum approach distance — magic threshold, harder to reason about than strict "<" comparison.

### B. feasibility

**Decision:** continuous formula via margin above viability threshold.

```rust
// Current (binary):
let feasibility = if ann.viability.passed { 1.0 } else { 0.0 };

// Proposed (continuous, with explicit !passed guard):
let feasibility = if !ann.viability.passed {
    0.0
} else {
    ((ann.viability.adjusted_score - VIABILITY_THRESHOLD) / FEASIBILITY_MARGIN)
        .clamp(0.0, 1.0)
};
```

> **Why the explicit guard:** `adjusted_score` for failed plans is **not specified** — it might equal the raw pre-viability score (potentially high, even when the plan was filtered for being unviable). Without the `!passed` guard, a failed plan with high raw score would compute `feasibility = 1.0` — semantically wrong. The legacy formula relied on `passed` boolean; the continuous version must preserve this binary cutoff at the failure boundary.

**Day-1 prerequisite — verify `adjusted_score` domain.** Before locking `FEASIBILITY_MARGIN`, sample `ann.viability.adjusted_score` across the pool on existing v32 logs (or via a small mining script). Record `min / p10 / p50 / p90 / max`. If the scale is materially different from `[0, 1]` (e.g., `[0, 100]` raw points), adjust margin to match — otherwise `MARGIN = 1.0` either re-collapses feasibility to `1.0` (too coarse) or stuck near `0` (too tight). **Implementer must paste this distribution into the 11.8 acceptance summary** before any formula tuning.

**Constants — v1 values set from P1 distribution data (see P1 results below):**
- `VIABILITY_THRESHOLD = 0.0` — **P2 confirmed `viability_min` field absent in tuning**. Use `0.0` unconditionally. **Do NOT add a new threshold tuning constant.**
- `FEASIBILITY_MARGIN = 2.0` — **data-driven from P1**: `adjusted_score` domain is `[-2.11, +3.99]`, **not** `[0, 1]` as initially assumed. Margin scan showed:

  | margin | clamp@1 | clamp@0 | middle_mass | verdict |
  |---|---|---|---|---|
  | 0.5 | 64% | 21% | 14% | fails gate |
  | 1.0 | 46% | 21% | 30% | technically passes, but ~half of plans saturate at 1.0 |
  | **2.0** | **11%** | 21% | **63%** | **chosen — clean continuous signal** |
  | 3.0 | 1% | 21% | 73% | over-compresses upper range (p90 ≈ 0.52) |

  `2.0` keeps the upper quality plans distinguishable while preserving wide middle mass. The single new tuning knob is `tuning.intent.feasibility_margin: f32 = 2.0`.

> **Note:** `FEASIBILITY_MARGIN = 1.0` is a **v1 scale assumption**, not a calibrated constant. Mining on the post-11.8 corpus may show that 1.0 is too generous (mean → 1.0 again) or too tight (mean → 0); adjust based on observed distribution.

**Tuning surface — strict minimum:** the only new tuning constant in 11.8 is `tuning.intent.feasibility_margin: f32 = 2.0`. `VIABILITY_THRESHOLD = 0.0` (P2-verified absent from current tuning, no new constant added). Don't introduce a constellation of knobs.

**Acceptance — middle-mass criterion (rules out bimodal):** rerun H1c on post-11.8 corpus must show genuine continuous spread, not collapse to extremes:
- `middle_mass = fraction of values in (0.05, 0.95)` ≥ **20%**;
- **OR** `p25 > 0.05 AND p75 < 0.95`.

`stddev` is reported as **diagnostic only** (not a pass criterion) because bimodal "50% at 0 / 50% at 1" satisfies `stddev = 0.5 > 0.15` while still being degenerate.

**Source-fix escalation rule:** if `adjusted_score` itself proves to be effectively binary or has a tiny dynamic range (Day-1 prerequisite check shows e.g., 95% of values clustered at one or two points) — **do not tune `FEASIBILITY_MARGIN` blindly to satisfy the gate**. The formula cannot fix a binary input. Escalate to a separate `viability.adjusted_score` source fix slice; that becomes a 11.8 sub-task or a 11.8.1 follow-up.

### C. leverage (intent-kind-aware)

**Decision:** different formulas per `IntentKind`. The current `0.5 × secure_kill + 0.5 × damage_norm` is **structurally wrong** for non-offensive intents.

> **Why leverage reuses some signals from `score_initial`.** Leverage formulas below intentionally use signals (damage, CC turns, secure_kill, terminal positional factors) that are **already** present in `score_initial` via `finalize_scores`. This is **not redundant** — it's a deliberate **item/target-relative reweighting**:
>
> - **Base score** answers: *"is this plan good in absolute terms?"* — uses batch-normalised raw factors.
> - **Leverage** answers: *"is this plan good for **this** agenda item / target?"* — uses target-relative ratios.
>
> Example: 30 damage on a 100 HP target gives `damage_ratio = 0.3`; 30 damage on a 30 HP target gives `damage_ratio = 1.0` (kill). Both produce similar `score_initial.Damage` (raw 30) but very different leverage. The `cdot × W_intent` bonus weights this perspective by one factor's worth — bounded so it cannot override the absolute scoring.
>
> **Mining acceptance must distinguish "leverage shifts ranking when target HP varies" from "leverage just amplifies absolute damage."** If H1c shows leverage strongly correlated with raw damage across all targets — the perspective is collapsing and the formula needs re-thinking.

**Weights as v1 priors:** all leverage weights below are **v1 priors**, not calibrated constants. Acceptance check: each leverage formula must produce non-degenerate distributions in mining; if one subcomponent dominates >80% of the formula's output, split it or retune the weights. Weights live as **named constants** at module scope of `overlay_considerations.rs`, not inline literals — no magic numbers buried in match-arms.

```rust
// Module-level constants (v1 priors, mining-adjustable):
const FOCUS_DAMAGE_WEIGHT: f32 = 0.6;
const FOCUS_KILL_WEIGHT: f32   = 0.4;
const APPLY_CC_REFERENCE_TURNS: f32 = 2.0; // 2 turns of CC = full leverage
const PROTECT_HEAL_WEIGHT: f32 = 0.6;
const PROTECT_CC_WEIGHT: f32   = 0.4;
const SELF_SURVIVAL_WEIGHT: f32 = 0.7;
const SELF_REDUCTION_WEIGHT: f32 = 0.3;
const REPO_LINE_WEIGHT: f32    = 0.5;
const REPO_CLUSTER_WEIGHT: f32 = 0.5;
const LAST_STAND_KILL_WEIGHT: f32 = 0.7;
const LAST_STAND_DAMAGE_WEIGHT: f32 = 0.3;
const LAST_STAND_DAMAGE_REFERENCE: f32 = 10.0; // 10 HP of damage = full credit (payoff-only)

let leverage = match item.kind {
    IntentKind::FocusTarget => {
        // Offensive: damage to **this specific target** relative to its HP + kill pressure.
        // Critical: use per-entity damage, not total enemy_damage — AoE plans must not
        // get full credit by hitting other enemies.
        // For single-target casts: matches Cast.target == item.target via plan/outcome alignment.
        // For AoE casts: looks up `enemy_damage_per_entity[item.target]`.
        let target_hp = target_current_hp_or_max(snap, item.target);
        let damage_to_target = damage_to_specific_target(plan, &ann.outcomes, item.target);
        let damage_ratio = if target_hp > 0.0 {
            (damage_to_target / target_hp).clamp(0.0, 1.0)
        } else { 0.0 };
        let kill = ann.terminal.get(TerminalFactor::SecureKill).clamp(0.0, 1.0);
        (FOCUS_DAMAGE_WEIGHT * damage_ratio + FOCUS_KILL_WEIGHT * kill).clamp(0.0, 1.0)
    }
    IntentKind::ApplyCC => {
        // Lock-down: CC duration applied to **this specific target**.
        // ApplyCC's purpose is incapacitation, not damage — a plan that CCs the
        // target for 2 turns must out-leverage a plan that deals 30 damage but
        // applies no CC. Damage is incidental here.
        //
        // v1 semantic: target-specific CC is inferred by step/outcome alignment
        // (Cast steps whose `target == item.target`). AoE or position-targeted
        // CC that hits the agenda target as a side-effect is **under-credited**
        // because per-entity CC breakdown is not currently in `ActionOutcomeEstimate`.
        // See backlog: per-entity CC breakdown for AoE/area CC leverage.
        let target_cc = cc_turns_applied_to_target(plan, &ann.outcomes, item.target);
        (target_cc / APPLY_CC_REFERENCE_TURNS).clamp(0.0, 1.0)
    }
    IntentKind::ProtectAlly => {
        // Rescue value: heal restored + threat reduced for ally.
        // v1 simplification: cc_value uses broad cc_turns_applied across all enemies,
        // not threat-specific. Any CC indirectly reduces team-wide threat. If mining
        // shows gaming (e.g., CC on unrelated enemy gets full rescue credit) — refine
        // to threat-specific in v2 via `ally_threat_proxy`.
        let heal = sum_hp_restored(&ann.outcomes);
        let ally_deficit = ally_hp_deficit_for_target(snap, item.target);
        let heal_ratio = if ally_deficit > 0.0 {
            (heal / ally_deficit).clamp(0.0, 1.0)
        } else { 0.0 };
        let cc_value = sum_cc_turns_applied(&ann.outcomes).clamp(0.0, 1.0);
        (PROTECT_HEAL_WEIGHT * heal_ratio + PROTECT_CC_WEIGHT * cc_value).clamp(0.0, 1.0)
    }
    IntentKind::ProtectSelf => {
        // Survival swing: SelfSurvival factor + danger reduction from start to plan-end.
        // CRITICAL: `danger_now` MUST read from actor's start-of-turn position
        // (snapshot active.pos), NOT from any post-plan or sim-mutated position —
        // otherwise reduction comparison is meaningless.
        // `ctx.scoring.active` is the UnitSnapshot taken at the start of the actor's
        // turn; `active.pos` is the start position. Verified invariant — but do not
        // refactor away from this source without re-checking.
        //
        // Intentional cap: stationary defensive plans (Cast self-shield without movement)
        // have `reduction = 0` → leverage maxes at SELF_SURVIVAL_WEIGHT (0.7), never 1.0.
        // Reaching full leverage (1.0) requires both buff effect AND active escape from
        // danger. This rewards mobile defense over passive defense — by design.
        // Do not "fix" by rebalancing weights to (1.0, 0.0); that erases the escape signal.
        let self_survival = ann.factors.get_plan(PlanFactor::SelfSurvival).clamp(0.0, 1.0);
        let danger_now = ctx.scoring.maps.danger.get(ctx.scoring.active.pos);
        let danger_after = ann.terminal.get(TerminalFactor::ExposureAtEnd);
        let reduction = (danger_now - danger_after).max(0.0).clamp(0.0, 1.0);
        (SELF_SURVIVAL_WEIGHT * self_survival + SELF_REDUCTION_WEIGHT * reduction).clamp(0.0, 1.0)
    }
    IntentKind::Reposition | IntentKind::SetupAOE => {
        // Positional gain: terminal LineActionability + cluster score.
        let line = ann.terminal.get(TerminalFactor::LineActionability).clamp(0.0, 1.0);
        let cluster = ann.terminal.get(TerminalFactor::PressureSpacingZone).clamp(0.0, 1.0);
        (REPO_LINE_WEIGHT * line + REPO_CLUSTER_WEIGHT * cluster).clamp(0.0, 1.0)
    }
    IntentKind::LastStand => {
        // LastStand leverage is **payoff-only**: kill pressure + damage pressure.
        // Must NOT include caster safety, defensive value, or survival penalty —
        // by definition the actor accepts death for offensive payoff.
        //
        // Note: LastStand uses **total** enemy_damage (not target-specific),
        // because LastStand payoff is **global trade value** ("how much hurt
        // before I die?"), not per-agenda-target leverage. Intentional asymmetry
        // with FocusTarget/ApplyCC.
        //
        // This branch exists for enum coverage; LastStand is not expected to
        // dominate normal agenda flow (it's adaptation-mode-triggered).
        let kill = ann.terminal.get(TerminalFactor::SecureKill).clamp(0.0, 1.0);
        let damage_norm = (sum_enemy_damage(&ann.outcomes) / LAST_STAND_DAMAGE_REFERENCE)
            .clamp(0.0, 1.0);
        (LAST_STAND_KILL_WEIGHT * kill + LAST_STAND_DAMAGE_WEIGHT * damage_norm).clamp(0.0, 1.0)
    }
};
```

**Helpers required (all single-file additions; no schema/struct changes):**

- `target_current_hp_or_max(snap, target_opt) -> f32` — reads target unit HP from snapshot, returns 0 when target=None.
- `damage_to_specific_target(plan, outcomes, target_opt) -> f32` — walks `plan.steps.zip(outcomes)`. For each `Cast` step:
  - If `outcome.enemy_damage_per_entity` is non-empty (AoE case): look up entry where `entity == target`, sum its damage.
  - Else (single-target case) and `Cast.target == target`: use `outcome.enemy_damage` directly.
  - Else: 0 contribution.
- `cc_turns_applied_to_target(plan, outcomes, target_opt) -> f32` — **v1 simplification**: walks `plan.steps.zip(outcomes)`, filters Cast steps where `Cast.target == target`, sums `outcome.cc_turns_applied`. **Does NOT require new struct fields.**

  > **v1 limitation — under-credits AoE/area CC.** A plan that targets entity A with an AoE that also stuns target B (the agenda target) will under-credit ApplyCC leverage, because we only credit when the explicit Cast target matches. Per-entity CC breakdown (`cc_turns_per_entity` field, analogous to `enemy_damage_per_entity`) is **deferred to backlog** — add only if mining shows ApplyCC under-credit is causing wrong attribution.
- `sum_hp_restored`, `sum_cc_turns_applied` — straightforward `Σ` over `ann.outcomes` (broad sum, used by ProtectAlly v1 simplification).
- `ally_hp_deficit_for_target(snap, ally_opt) -> f32` — `ally.max_hp - ally.hp` for the protected ally.

**Removed:**
- Global magic constant `LEVERAGE_SOFT_MAX = 5.0` — replaced by per-intent target-relative normalization.
- One-size-fits-all `0.5 × secure_kill + 0.5 × damage_norm` — replaced by 6 intent-specific branches.

**Acceptance — per-IntentKind histograms (mining enhancement required):** the existing global `leverage` H1c histogram is **not sufficient** — it averages across all intent kinds and hides per-kind biases. After 11.8, mining must be extended to emit a **per-IntentKind leverage histogram** for each of the 6 branches (FocusTarget / ApplyCC / ProtectAlly / ProtectSelf / Reposition / SetupAOE / LastStand). Acceptance criterion:

- Each per-kind histogram is non-degenerate (same middle-mass criterion as feasibility: ≥ 20% mass in `(0.05, 0.95)` OR `p25 > 0.05 AND p75 < 0.95`).
- **Cross-kind balance:** if any one IntentKind's mean leverage exceeds others' mean by > 30% (relative), flag for retune in v2 — that intent's items would systematically dominate cdot competition in multi-item agendas.

This adds a small mining-side task to Day 3 (Section "Scope decision") — extend H1c to break leverage by `agenda.items[i].kind`, output one histogram per kind.

### D. safety

**Decision:** keep current formula, document corpus-bound flatness as expected behavior, add verification probe.

```rust
// Unchanged from 11.4:
let safety = 1.0 - self_damage_ratio.max(exposure);
```

**Documentation:** add module-level comment in `overlay_considerations.rs::safety_section` explaining that flat distribution is expected when corpus avoids dangerous tiles (OvercommitIntoDanger critic + safe scenario design). Cite 11.7b synthetic tests as proof of correctness.

**Verification probe (in 11.8 acceptance):** **unit / synthetic test** in `pipeline/stages/overlay_considerations.rs::tests` (NOT an `ai_scenarios` fixture — those are blocked behind 11.9 fixture rebuild). Construct a minimal synthetic scenario:
- One actor in a tile with `maps.danger.get(actor_pos)` non-zero (e.g., adjacent to a high-threat enemy).
- Build a plan that ends in that tile (so `terminal.exposure_at_end > 0`).
- Run `OverlayConsiderationsStage`.
- Assert `per_item.considerations.safety < 1.0`.

If probe passes → formula confirmed working; the H1c flatness in production corpus is corpus-bound, as expected.

If probe fails (safety=1.0 even when exposure > 0) — formula is broken, escalate to backlog as separate slice.

### E. HardRescue — status note (not in 11.8 scope)

**Status:** already fixed in commit `41579a5` before 11.8.

`build_hard_rescue_opportunity` now returns N=1 ProtectAlly only when no threat target is identified. Regression test `agenda_hard_rescue_skips_focus_target_when_no_threat` pins the corrected semantic.

11.8 assumes agenda-item target construction is valid (no `target=None` for FocusTarget kinds). No further work needed for HardRescue in this slice.

## Implementation plan (steps for executor)

**Selected scope: Structural changes** (intent-kind-aware leverage, ApproachTarget eligibility for Forced, continuous feasibility, safety probe).

Total estimate: **~3 days**. Strict sequencing: P-steps first (prerequisites — investigation only), then S/T/U (implementation). Each step has explicit acceptance.

### Day 0 / Prerequisites — completed before implementer launch

> All P-steps **already done in conversation**. Outputs and decisions recorded below; no investigation needed by the implementer. Implementer can start directly with Day 1 (S-steps).

**P1 — `adjusted_score` distribution sampled on existing v32 logs (n=3557 plans, 3230 passed):**

```
domain:     [-2.11, +3.99]   ← NOT [0, 1] as initially assumed
mean:        0.86  stddev: 0.97
percentiles: p10=-0.32  p25=0.15  p50=0.89  p75=1.55  p90=2.07
```

21% of "passed" plans have negative `adjusted_score` (clamped to 0 by formula). Margin scan led to the choice **`FEASIBILITY_MARGIN = 2.0`** (see Section B for full table). With this margin: 11% clamp@1, 21% clamp@0, 63% middle_mass — clean continuous signal.

**P2 — `viability_min` field absent.** `ya tool ast-index agrep "viability_min"` returned 0 matches. Decision: `VIABILITY_THRESHOLD = 0.0` unconditionally, **no new tuning constant added**.

**P3 — `cc_turns_per_entity` field absent, but NOT needed.** `ActionOutcomeEstimate` has only aggregate `cc_turns_applied: f32`. Instead of adding a new field, the helper `cc_turns_applied_to_target` uses `plan.steps.zip(outcomes)` and filters Cast steps where `Cast.target == agenda.target`. Same trick used by `damage_to_specific_target` (which falls back to `enemy_damage_per_entity` for AoE). **Net result: zero schema/struct/log/mining changes for cc handling.** v1 limitation: AoE/area CC affecting the agenda target as a side-effect is under-credited — backlog item if mining shows ApplyCC under-credit.

**P4 — `active.pos` invariant verified.** `ScoringCtx.active = snap.unit(actor)`; `snap: BattleSnapshot` is taken at start of actor's turn and is immutable during planning (planning is hypothetical, doesn't mutate snapshot). Therefore `ctx.scoring.active.pos` reliably equals start-of-turn position — safe to use in `ProtectSelf` leverage formula's `danger_now`.

### Day 1 / Leverage formulas (S-steps)

- **S1.** Add 11 named constants at module scope of `overlay_considerations.rs`: `FOCUS_DAMAGE_WEIGHT`, `FOCUS_KILL_WEIGHT`, `APPLY_CC_REFERENCE_TURNS`, `PROTECT_HEAL_WEIGHT`, `PROTECT_CC_WEIGHT`, `SELF_SURVIVAL_WEIGHT`, `SELF_REDUCTION_WEIGHT`, `REPO_LINE_WEIGHT`, `REPO_CLUSTER_WEIGHT`, `LAST_STAND_KILL_WEIGHT`, `LAST_STAND_DAMAGE_WEIGHT`, `LAST_STAND_DAMAGE_REFERENCE`. Values per Section C code listing.
- **S2.** Implement helpers (location: same file or shared module — implementer's choice, but no duplication). All take `&TurnPlan` where step/outcome alignment is needed:
  - `target_current_hp_or_max(snap, target_opt) -> f32`
  - `damage_to_specific_target(plan, outcomes, target_opt) -> f32` — walks `plan.steps.zip(outcomes)`; uses `enemy_damage_per_entity` for AoE, falls back to `enemy_damage` for single-target where `Cast.target == target`
  - `cc_turns_applied_to_target(plan, outcomes, target_opt) -> f32` — walks `plan.steps.zip(outcomes)`, filters `Cast.target == target`, sums `outcome.cc_turns_applied`. **No new struct field needed** (per P3 decision). Documents v1 limitation: AoE/area CC on non-explicit-targets under-credited.
  - `sum_hp_restored(outcomes) -> f32`
  - `sum_cc_turns_applied(outcomes) -> f32`
  - `ally_hp_deficit_for_target(snap, ally_opt) -> f32`
- **S3.** Replace single-formula leverage in `OverlayConsiderationsStage` with the 6-branch `match item.kind` per Section C listing. Remove old `LEVERAGE_SOFT_MAX = 5.0`.
- **S4.** Unit tests — one per leverage branch (6 total): `leverage_focus_target`, `leverage_apply_cc`, `leverage_protect_ally`, `leverage_protect_self`, `leverage_reposition_or_setup_aoe`, `leverage_last_stand`. Each constructs a synthetic snapshot + plan annotation, asserts expected leverage value.
- **S5.** Two negative tests for target-specificity:
  - `leverage_focus_target_aoe_does_not_credit_other_enemies` — FocusTarget item with target=A; plan AoE hits B and C but not A → `damage_to_target = 0` → leverage close to 0 (only kill component if any).
  - `leverage_apply_cc_aoe_cc_does_not_credit_other_enemies` — symmetric for CC.

### Day 2 / Eligibility + Feasibility (T-steps)

- **T1.** Add `tuning.intent.feasibility_margin: f32 = 1.0` to `AiTuning` (the **only** new tuning constant in 11.8). Update tuning struct, default, and any TOML parser.
- **T2.** Implement continuous feasibility in `OverlayConsiderationsStage` per Section B code (with `!ann.viability.passed` guard as first branch).
- **T3.** Add pool-level pre-pass at the start of `ItemScoringStage::apply()`: scan post-Viability pool once, compute `pool_has_no_offensive_plan_vs_taunter: bool`. Cache in stage-local variable (no `StageCtx` field needed if pre-pass and per-plan loop are in the same stage).
- **T4.** Implement ApproachTarget eligibility inside the per-plan loop in `ItemScoringStage`. Composite eligibility:
  - `plan_is_offensive_vs(plan, target)` returns true → eligible (primary path).
  - **Else if** `band == ForcedTargeting && pool_has_no_offensive_plan_vs_taunter && approaches_target(plan, target_pos)` → eligible (fallback path).
  - Otherwise → not eligible.
  - `target_pos = snap.unit(item.target).pos`.
  - `approaches_target` per Section A definition (hex distance + Move step + viability passed).
- **T5.** Tests:
  - `approach_target_eligible_when_forced_and_no_offensive_in_pool` (positive)
  - `approach_target_ineligible_when_forced_but_offensive_plan_in_pool` (pool-level fallback respected)
  - `approach_target_ineligible_in_normal_tactical_band` (band scope respected)
  - `feasibility_continuous_distinguishes_two_adjusted_scores` (two scores just above threshold by different margins → different feasibility outputs)
  - `feasibility_zero_when_viability_failed` (pin the `!passed` guard)

### Day 3 / Mining + safety probe + acceptance (U-steps)

- **U1.** Extend `mine_ai_logs.rs` H1c output: split global `leverage` histogram into **per-IntentKind** histograms (one per match-arm: FocusTarget, ApplyCC, ProtectAlly, ProtectSelf, Reposition/SetupAOE, LastStand). Apply same percentile + middle-mass reporting per kind.
- **U2.** Synthetic safety probe unit test in `pipeline/stages/overlay_considerations.rs::tests` per Section D. Construct minimal scenario: actor at tile with `maps.danger.get(actor_pos) > 0`, plan ends at that tile (`exposure_at_end > 0`), run overlay, assert `safety < 1.0`.
- **U3.** **Manual: collect new v32 logs** with 11.8 changes deployed (NOT an executor task — flag to user that this must happen between U2 and U4). User runs playtest, executor pauses.
- **U4.** Run `cargo run --release --bin mine_ai_logs -- --dir logs/` on new corpus. Verify acceptance gates:
  - **Gate 1** (distribution): middle-mass criterion satisfied for feasibility, global leverage, and per-IntentKind leverage histograms; safety probe passes.
  - **Gate 2** (eligibility): Forced fallback ≤ 35% OR ≥ 20% relative reduction.
  - **Gate 3** (unit tests): all ~13 new tests green.
  - **Gate 4** (no regression): `cargo test --lib` green.
- **U5.** Append findings to acceptance summary (in design doc or commit message): paste H1c output, comparison vs baseline, list any per-axis decisions deferred. Update Follow-ups if mining surfaces new items.

### Defer to backlog

- Stricter approach rule (`can_cast_next_turn`) — current simple distance rule first.
- Mining-driven calibration of `FEASIBILITY_MARGIN` — wait for first run.
- Per-band leverage weight tuning — wait for first run.
- ProtectAlly threat-specific CC (v1 uses broad sum).
- continuation_value `p99 ≈ 0.5` investigation.

## Acceptance gates for 11.8

> **Sample-size note:** N=211 corpus is calibration smoke. Treat numerical thresholds as approximate; ±5–10pp from a gate is the noise floor. Direction matters more than literal pass/fail.

1. **Distribution gate (middle-mass criterion, sync with Section B):** post-11.8 H1c rerun on **new** logs shows that `feasibility` and global `leverage` are not collapsed to extremes. For each axis:
   - `middle_mass = fraction of values in (0.05, 0.95)` ≥ **20%**;
   - **OR** `p25 > 0.05 AND p75 < 0.95`.
   - `stddev` is reported as diagnostic only (not a pass criterion — bimodal "50% at 0 / 50% at 1" satisfies `stddev = 0.5` while still being degenerate).
   - `safety` may stay flat (corpus-bound) **provided** the synthetic safety probe (axis D) passes.
   - **Per-IntentKind leverage histograms** (mining enhancement, see axis C acceptance) must each satisfy the same middle-mass criterion. Cross-kind balance: no kind's mean leverage exceeds another's by > 30% relative.
2. **Eligibility gate (Forced):** primary criterion `ForcedTargeting fallback rate ≤ 35%` **OR** `relative reduction ≥ 20%` from baseline 45.9% (i.e., post-11.8 rate ≤ 36.7%). The two paths are *meaningfully distinct*: Path 1 is an absolute target; Path 2 admits a partial improvement when an absolute target is just out of reach. Secondary criterion: ApproachTarget attribution appears in ForcedTargeting cases AND does not increase Skip/EndTurn rate. Rationale: some Forced taunters are physically hopeless this turn — gate must not penalise that.

   NormalTactical fallback rate is allowed to stay similar (it's physics, not a calibration problem).
3. **Unit-test gate (explicit minimums):**
   - 1 test per leverage branch — 6 branches: FocusTarget, ApplyCC, ProtectAlly, ProtectSelf, Reposition/SetupAOE, LastStand → 6 tests;
   - 3 tests for ApproachTarget eligibility — (a) Forced + no offensive plan in pool + plan approaches taunter → eligible, (b) Forced + offensive plan present in pool + plan-only-approaches → NOT eligible (pool-level fallback respected), (c) NormalTactical + plan approaches target → NOT eligible (band scope respected);
   - 1 test verifying continuous feasibility (input: two `adjusted_score` values just above threshold by different margins → produces different `feasibility` outputs);
   - 1 synthetic safety probe test (per axis D);
   - 1 test for target-specific damage (FocusTarget AoE plan that hits other enemies must NOT get full leverage credit);
   - 1 test for target-specific CC (ApplyCC: AoE CC on non-target enemy must NOT credit ApplyCC leverage on `item.target`).
   - **Total: ~13 new unit tests.**
4. **No regression:** `cargo test --lib` green. Existing `ai_scenarios` and `replay_assert` failures (pre-existing v30/v32 schema mismatch) are out of scope for 11.8 — they belong to 11.9 fixture rebuild.

> **Out of scope for 11.8:** `ai_scenarios` golden review and `replay_assert` regressions. These depend on 11.9 fixture rebuild on new v32+ logs collected after 11.8 deploys. Sequence: 11.8 implements → mining gates pass → 11.9 collects new logs and rebuilds fixtures → behavior diff per-fixture analysis happens in 11.9.

## Follow-ups

- **HardRescue/FocusTarget target=None** — fixed in `41579a5`.
- **Stricter approach rule** (`can_cast_next_turn(plan, target)`) — backlog after first round of ApproachTarget mining.
- **Per-band leverage tuning** — backlog after first 11.8 mining run.
- **`FEASIBILITY_MARGIN` calibration** — backlog after first 11.8 mining run.
- **safety probe** — implemented in 11.8 (axis D); if it fails, separate fix slice.
- **ProtectAlly threat-specific CC** — v1 uses broad `cc_turns_applied`. Refine to threat-specific (only count CC on enemies threatening the protected ally) if mining shows gaming behavior.
- **Per-entity CC breakdown for AoE leverage** — v1 `cc_turns_applied_to_target` matches by Cast.target only; AoE/area CC that hits the agenda target as a side-effect is under-credited. If mining shows ApplyCC under-credit (e.g., low ApplyCC leverage despite plans that demonstrably stun the target via AoE), add `cc_turns_per_entity: Vec<(Entity, f32)>` to `ActionOutcomeEstimate` (analogous to `enemy_damage_per_entity`).
- **continuation_value rare > 0.5** — H1c shows `p99=0.499`, suggesting `repair_affinity.severity_factor` rarely fires. Investigate why repair_affinity is mostly zero. Backlog after 11.8 mining.
- **Step 12+ removal of deprecated `select_intent`** — already deprecated in 11.5; full removal scheduled for step 12.
