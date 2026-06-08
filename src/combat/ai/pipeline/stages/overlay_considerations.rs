//! OverlayConsiderationsStage — step 11.4 / 11.8.
//!
//! Updates `ann.per_item[i].considerations` with plan-aware data for each
//! plan × agenda-item pair.  Must run **after** `RepairAffinityStage` so that
//! `ann.repair_affinity` is populated and the `continuation_value` axis is
//! accurate; before `PlanModifiersStage` so the composition in `PickBestStage`
//! uses the most accurate data.
//!
//! # What it overlays
//!
//! - **feasibility** — `ann.viability.adjusted_score.clamp(0,1)` (continuous).
//! - **leverage**    — intent-kind-aware formula (6-branch match per item.kind).
//!   See Section C of the 11.8 design doc for per-branch formulas and rationale.
//! - **safety**      — `1.0 - max(self_damage_ratio, exposure_at_end)`.
//!   Expected to stay near 1.0 in safe corpora (OvercommitIntoDanger critic +
//!   safe scenario design). See 11.7b finding and Section D of design doc.
//! - **continuation_value** — recomputes using `ann.repair_affinity` (plan-level).
//!
//! Other axes (`urgency`, `role_affinity`) are taken verbatim from the
//! item-level `AgendaItem::considerations` populated by `build_agenda` in 11.3.
//!
//! # Edge cases
//!
//! - Empty agenda → no-op.
//! - Empty `ann.per_item` (no `ItemScoringStage` ran) → no-op.

use bevy::prelude::Entity;

use crate::combat::ai::intent::agenda::AgendaItem;
use crate::combat::ai::intent::considerations::IntentConsiderations;
use crate::combat::ai::intent::IntentKind;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::factors::{
    FactorTerminalScore, PlanFactor, PlanFactorValues, TerminalFactor,
};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::game::hex::Hex;

// ── Leverage v1 priors (mining-adjustable) ─────────────────────────────────
/// Viability threshold below which a plan is considered non-viable.
/// P2-verified: `viability_min` field absent in AiTuning — use 0.0 unconditionally.
/// Do NOT replace with a tuning knob (design doc Section B).
const VIABILITY_THRESHOLD: f32 = 0.0;

const FOCUS_DAMAGE_WEIGHT: f32 = 0.6;
const FOCUS_KILL_WEIGHT: f32 = 0.4;
const APPLY_CC_REFERENCE_TURNS: f32 = 2.0; // 2 turns of CC = full leverage
const PROTECT_HEAL_WEIGHT: f32 = 0.6;
const PROTECT_CC_WEIGHT: f32 = 0.4;
const SELF_SURVIVAL_WEIGHT: f32 = 0.7;
const SELF_REDUCTION_WEIGHT: f32 = 0.3;
const REPO_LINE_WEIGHT: f32 = 0.5;
const REPO_CLUSTER_WEIGHT: f32 = 0.5;
const LAST_STAND_KILL_WEIGHT: f32 = 0.7;
const LAST_STAND_DAMAGE_WEIGHT: f32 = 0.3;
const LAST_STAND_DAMAGE_REFERENCE: f32 = 10.0; // 10 HP of damage = full credit (payoff-only)

pub struct OverlayConsiderationsStage;

impl PlanStage for OverlayConsiderationsStage {
    fn name(&self) -> &'static str {
        "overlay_considerations"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(agenda) = ctx.agenda else { return };
        if agenda.items.is_empty() || pool.is_empty() {
            return;
        }

        let actor_max_hp = ctx.scoring.active.max_hp() as f32;

        let n_plans = pool.plans.len();
        for plan_idx in 0..n_plans {
            let ann = &pool.annotations[plan_idx];

            if ann.per_item.is_empty() {
                continue;
            }

            // Compute plan-aware axis values once per plan.

            // feasibility: continuous formula via margin above viability threshold.
            //
            // The `!passed` guard is the critical branch: `adjusted_score` for
            // failed plans is not specified (may be a raw pre-viability score that
            // is high even though the plan is unviable). Without this guard a
            // failed plan with high `adjusted_score` would compute feasibility=1.0,
            // which is semantically wrong. See Section B of ai_rework_step11_8_design.md.
            //
            // VIABILITY_THRESHOLD = 0.0 (P2-verified — viability_min field absent
            // in AiTuning). feasibility_margin = 2.0 (data-driven from P1 sampling:
            // adjusted_score domain [-2.11, +3.99]; margin 2.0 → 63% middle-mass).
            let feasibility = if !ann.viability.passed {
                0.0
            } else {
                let margin = ctx.scoring.world.tuning.intent.feasibility_margin;
                ((ann.viability.adjusted_score - VIABILITY_THRESHOLD) / margin).clamp(0.0, 1.0)
            };

            // safety: 1.0 - max(self_damage_ratio, exposure_at_end).
            // self_damage_ratio: cumulative self-damage normalised by actor's max HP.
            // ExposureAtEnd: danger at the plan's final position (terminal factor).
            // Expected flat (≈1.0) in safe corpora — see 11.7b finding and Section D.
            //
            // Outcomes live on `TurnPlan.annotation` (populated by generator); the
            // `outcomes` field on pipeline annotation is dead during pipeline.
            // See `ScoredPool::plan_outcomes` for the canonical accessor.
            let total_self_damage: f32 = pool
                .plan_outcomes(plan_idx)
                .iter()
                .map(|o| o.self_damage)
                .sum();
            let self_damage_ratio = if actor_max_hp > f32::EPSILON {
                (total_self_damage / actor_max_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let exposure = ann
                .terminal
                .get(TerminalFactor::ExposureAtEnd)
                .clamp(0.0, 1.0);
            let safety = 1.0 - self_damage_ratio.max(exposure);

            // continuation_value: recompute with plan-level repair_affinity.
            // Formula: 0.5 × continue_commitment + 0.5 × repair_severity_factor
            let repair_affinity = &ann.repair_affinity;
            let commitment = ctx.scoring.need_signals.continue_commitment.clamp(0.0, 1.0);
            let repair_score = repair_affinity.severity_factor.clamp(0.0, 1.0);
            let continuation_value = if ctx.scoring.last_goal.is_some() {
                (0.5 * commitment + 0.5 * repair_score).clamp(0.0, 1.0)
            } else {
                (0.5 * commitment).clamp(0.0, 1.0)
            };

            // Phase 1: compute leverage per item (immutable borrows only — no clones).
            // Borrow scope ends before phase 2's mutation.
            let leverages: Vec<f32> = {
                let plan = &pool.plans[plan_idx];
                let ann = &pool.annotations[plan_idx];
                // Authoritative outcomes — pipeline annotation outcomes are dead
                // during pipeline (default-empty); see `ScoredPool::plan_outcomes`.
                let outcomes = plan.annotation.outcomes.as_slice();
                let terminal = ann.terminal;
                let factors = ann.factors;
                let n_per_item = ann.per_item.len();

                agenda
                    .items
                    .iter()
                    .enumerate()
                    .map(|(item_idx, item)| {
                        debug_assert!(
                            item_idx < n_per_item,
                            "ItemScoringStage must populate per_item to agenda.items.len()",
                        );
                        if item_idx >= n_per_item {
                            return 0.0;
                        }
                        compute_leverage(
                            item,
                            plan,
                            outcomes,
                            terminal,
                            factors,
                            ctx.scoring.snap,
                            ctx.scoring.maps,
                            ctx.scoring.active.pos,
                        )
                    })
                    .collect()
            };

            // Phase 2: write per-item overlaid considerations back (mutable borrow).
            let n_per_item = pool.annotations[plan_idx].per_item.len();
            for (item_idx, &leverage) in leverages.iter().enumerate() {
                if item_idx >= n_per_item {
                    continue;
                }
                let base = &agenda.items[item_idx].considerations;
                pool.annotations[plan_idx].per_item[item_idx].considerations =
                    IntentConsiderations {
                        urgency: base.urgency,             // item-level
                        feasibility,                       // plan-level
                        leverage,                          // plan-level, per-item-kind
                        safety,                            // plan-level
                        role_affinity: base.role_affinity, // item-level
                        continuation_value,                // plan-level
                    };
            }
        }
    }
}

// ── Leverage compute (pure function) ──────────────────────────────────────────

/// Compute the leverage axis for a single agenda item × plan pair.
///
/// Pure function: borrows everything immutably, no allocation. All inputs that
/// can come from unbounded sources are clamped to `[0, 1]` at the source. Final
/// clamps at the end of each branch are dropped where weights sum to `1.0`
/// (they would be no-ops); ApplyCC keeps its clamp because the divisor scales
/// an unbounded numerator.
///
/// See Section C of `docs/ai_rework_step11_8_design.md` for per-branch rationale.
#[allow(clippy::too_many_arguments)]
fn compute_leverage(
    item: &AgendaItem,
    plan: &TurnPlan,
    outcomes: &[ActionOutcomeEstimate],
    terminal: FactorTerminalScore,
    factors: PlanFactorValues,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    actor_pos: Hex,
) -> f32 {
    match item.kind {
        IntentKind::FocusTarget => {
            // Offensive: damage to **this specific target** relative to its HP +
            // kill pressure. Per-entity damage prevents AoE overcredit.
            let target_hp = target_current_hp_or_max(snap, item.target);
            let damage_to_target = damage_to_specific_target(plan, outcomes, item.target);
            let damage_ratio = if target_hp > 0.0 {
                (damage_to_target / target_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let kill = terminal.get(TerminalFactor::SecureKill).clamp(0.0, 1.0);
            FOCUS_DAMAGE_WEIGHT * damage_ratio + FOCUS_KILL_WEIGHT * kill
        }
        IntentKind::ApplyCC => {
            // Lock-down: CC duration on **this specific target**. v1 limitation:
            // only credits Cast steps whose target == agenda target; AoE/area CC
            // hitting the target as side-effect is under-credited (see backlog).
            //
            // Final clamp is required: numerator can exceed APPLY_CC_REFERENCE_TURNS.
            let target_cc = cc_turns_applied_to_target(plan, outcomes, item.target);
            (target_cc / APPLY_CC_REFERENCE_TURNS).clamp(0.0, 1.0)
        }
        IntentKind::ProtectAlly => {
            // Rescue value: heal restored relative to ally deficit + broad CC value.
            // v1 simplification: cc_value sums CC across all enemies, not threat-specific.
            let heal = sum_hp_restored(outcomes);
            let ally_deficit = ally_hp_deficit_for_target(snap, item.target);
            let heal_ratio = if ally_deficit > 0.0 {
                (heal / ally_deficit).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let cc_value = sum_cc_turns_applied(outcomes).clamp(0.0, 1.0);
            PROTECT_HEAL_WEIGHT * heal_ratio + PROTECT_CC_WEIGHT * cc_value
        }
        IntentKind::ProtectSelf => {
            // Survival swing: SelfSurvival factor + danger reduction.
            // CRITICAL: `danger_now` MUST read from actor's start-of-turn position
            // (snapshot active.pos), NOT from any post-plan or sim-mutated position —
            // otherwise reduction comparison is meaningless.
            //
            // Intentional cap: stationary defensive plans (Cast self-shield without
            // movement) have `reduction = 0` → leverage maxes at SELF_SURVIVAL_WEIGHT
            // (0.7), never 1.0. Reaching full leverage requires both buff effect AND
            // active escape from danger. This rewards mobile defense over passive
            // defense — by design. Do not "fix" by rebalancing weights to (1.0, 0.0);
            // that erases the escape signal.
            let self_survival = factors.get_plan(PlanFactor::SelfSurvival).clamp(0.0, 1.0);
            let danger_now = maps.danger.get(actor_pos).clamp(0.0, 1.0);
            let danger_after = terminal.get(TerminalFactor::ExposureAtEnd).clamp(0.0, 1.0);
            let reduction = (danger_now - danger_after).max(0.0); // both in [0,1] → diff in [-1,1], max(0) → [0,1]
            SELF_SURVIVAL_WEIGHT * self_survival + SELF_REDUCTION_WEIGHT * reduction
        }
        IntentKind::Reposition | IntentKind::SetupAOE => {
            // Positional gain: terminal LineActionability + cluster score.
            let line = terminal
                .get(TerminalFactor::LineActionability)
                .clamp(0.0, 1.0);
            let cluster = terminal
                .get(TerminalFactor::PressureSpacingZone)
                .clamp(0.0, 1.0);
            REPO_LINE_WEIGHT * line + REPO_CLUSTER_WEIGHT * cluster
        }
        IntentKind::LastStand => {
            // Payoff-only: kill pressure + total damage pressure. Must NOT include
            // safety/survival — actor accepts death for offensive payoff.
            //
            // Uses **total** enemy_damage (not target-specific): LastStand payoff is
            // global trade value, not per-agenda-target leverage. Intentional asymmetry
            // with FocusTarget/ApplyCC.
            let kill = terminal.get(TerminalFactor::SecureKill).clamp(0.0, 1.0);
            let damage_norm =
                (sum_enemy_damage(outcomes) / LAST_STAND_DAMAGE_REFERENCE).clamp(0.0, 1.0);
            LAST_STAND_KILL_WEIGHT * kill + LAST_STAND_DAMAGE_WEIGHT * damage_norm
        }
    }
}

// ── Leverage helpers ──────────────────────────────────────────────────────────

/// Returns current HP of the target unit from the battle snapshot.
/// Returns 0.0 when `target_opt` is `None` or unit not found.
fn target_current_hp_or_max(snap: &BattleSnapshot, target_opt: Option<Entity>) -> f32 {
    target_opt
        .and_then(|t| snap.unit(t))
        .map(|u| u.hp().max(0) as f32)
        .unwrap_or(0.0)
}

/// Returns total damage dealt to a specific target entity by this plan.
///
/// For AoE casts (`enemy_damage_per_entity` non-empty): looks up the target
/// entity in the per-entity vec. For single-target casts: credits `enemy_damage`
/// only when `Cast.target == target`. Move steps contribute 0.
fn damage_to_specific_target(
    plan: &TurnPlan,
    outcomes: &[ActionOutcomeEstimate],
    target_opt: Option<Entity>,
) -> f32 {
    let Some(target) = target_opt else {
        return 0.0;
    };
    plan.steps
        .iter()
        .zip(outcomes.iter())
        .filter_map(|(step, outcome)| match step {
            PlanStep::Cast {
                target: cast_target,
                ..
            } => {
                if !outcome.enemy_damage_per_entity.is_empty() {
                    // AoE: look up entity in per-entity vec
                    outcome
                        .enemy_damage_per_entity
                        .iter()
                        .find(|(e, _)| *e == target)
                        .map(|(_, d)| *d)
                } else if *cast_target == target {
                    // Single-target: full damage
                    Some(outcome.enemy_damage)
                } else {
                    None
                }
            }
            _ => None,
        })
        .sum()
}

/// Returns total CC turns applied to a specific target entity by this plan.
///
/// v1: only credits Cast steps whose explicit target == agenda target.
/// AoE/area CC on non-explicit targets is under-credited (backlog).
fn cc_turns_applied_to_target(
    plan: &TurnPlan,
    outcomes: &[ActionOutcomeEstimate],
    target_opt: Option<Entity>,
) -> f32 {
    let Some(target) = target_opt else {
        return 0.0;
    };
    plan.steps
        .iter()
        .zip(outcomes.iter())
        .filter_map(|(step, outcome)| match step {
            PlanStep::Cast { target: t, .. } if *t == target => Some(outcome.cc_turns_applied),
            _ => None,
        })
        .sum()
}

/// Sum of `hp_restored` across all plan outcomes.
fn sum_hp_restored(outcomes: &[ActionOutcomeEstimate]) -> f32 {
    outcomes.iter().map(|o| o.hp_restored).sum()
}

/// Sum of `cc_turns_applied` across all plan outcomes.
fn sum_cc_turns_applied(outcomes: &[ActionOutcomeEstimate]) -> f32 {
    outcomes.iter().map(|o| o.cc_turns_applied).sum()
}

/// Sum of `enemy_damage` across all plan outcomes (total, not target-specific).
fn sum_enemy_damage(outcomes: &[ActionOutcomeEstimate]) -> f32 {
    outcomes.iter().map(|o| o.enemy_damage).sum()
}

/// Returns HP deficit (max_hp - hp) of the protected ally.
/// Returns 0.0 when `ally_opt` is `None` or unit not found.
fn ally_hp_deficit_for_target(snap: &BattleSnapshot, ally_opt: Option<Entity>) -> f32 {
    ally_opt
        .and_then(|a| snap.unit(a))
        .map(|u| (u.max_hp().max(0) - u.hp().max(0)).max(0) as f32)
        .unwrap_or(0.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "overlay_considerations_tests.rs"]
mod tests;
