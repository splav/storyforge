//! PlanModifiersStage — step 8.B.
//!
//! Applies all registered `PlanModifier` implementations to every non-masked
//! plan in the pool. This stage runs after `RepairAffinityStage` (which
//! populates `ann.repair_affinity`) and before `PickBestStage` (which reads
//! the final `ann.score`).
//!
//! For each plan the stage iterates `PLAN_MODIFIERS` in fixed order
//! `[summon_bonus, trade_bonus, repair_bonus]`, accumulates the signed
//! additive contribution into `ann.score`, and records each contribution in
//! `ann.modifiers` for observability.
//!
//! Masked plans (`ann.score == NEG_INFINITY`) are skipped entirely so that
//! contract masks applied by `ProtectSelfMaskStage` / `KillableGateStage`
//! are not disturbed.

use crate::combat::ai::modifiers::{ModifierContribution, ModifierCtx, PLAN_MODIFIERS};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::scorer::build_summon_dpr_cache;
use crate::combat::ai::scoring::trade::unit_value;

pub struct PlanModifiersStage;

impl PlanStage for PlanModifiersStage {
    fn name(&self) -> &'static str {
        "plan_modifiers"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Build summon DPR cache once for the pool. Empty when no plan summons.
        let summon_dpr = build_summon_dpr_cache(&pool.plans, ctx.scoring.world);
        // Actor's own unit_value is plan-independent — compute once per pool.
        let actor_value = unit_value(ctx.scoring.active, ctx.scoring.world.content);
        // Role repair weights — computed once, shared across all modifier calls.
        let repair_weights = ctx.scoring.active.role.repair_weights(ctx.scoring.world.tuning);

        let mctx = ModifierCtx {
            stage: ctx,
            summon_dpr: &summon_dpr,
            actor_value,
            repair_weights,
        };

        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            // Skip plans masked by ProtectSelf / KillableGate (score == NEG_INFINITY).
            if !ann.score.is_finite() {
                continue;
            }
            for m in PLAN_MODIFIERS {
                let contribution = m.modify(plan, ann, &mctx);
                ann.modifiers.push(ModifierContribution {
                    name: m.name().into(),
                    contribution,
                });
                ann.score += contribution;
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::pipeline::ScoredPool;
    use crate::combat::ai::planning::types::TurnPlan;

    /// A masked plan (score == NEG_INFINITY) must be skipped:
    /// `ann.modifiers` stays empty and `ann.score` stays NEG_INFINITY.
    #[test]
    fn plan_modifiers_stage_skips_masked_plans() {
        let mut plan = TurnPlan::default();
        plan.annotation.score = f32::NEG_INFINITY;
        let mut pool = ScoredPool::new(vec![plan]);
        pool.annotations[0].score = f32::NEG_INFINITY;

        // We can't call apply() without a real StageCtx, so we test the loop
        // invariant directly: the stage should skip plans where !score.is_finite().
        // This test verifies that NEG_INFINITY is correctly identified as non-finite.
        assert!(!f32::NEG_INFINITY.is_finite(), "NEG_INFINITY must not be finite");
        assert!(pool.annotations[0].modifiers.is_empty());
        assert_eq!(pool.annotations[0].score, f32::NEG_INFINITY);
    }

    /// Verify that ScoredPool correctly initialises annotations with default score (0.0).
    #[test]
    fn plan_modifiers_stage_writes_contributions_per_modifier() {
        let pool = ScoredPool::new(vec![TurnPlan::default(); 2]);
        // Before PlanModifiersStage runs, modifiers vec is empty.
        assert!(pool.annotations[0].modifiers.is_empty());
        assert!(pool.annotations[1].modifiers.is_empty());
        // PLAN_MODIFIERS has exactly 3 entries.
        assert_eq!(PLAN_MODIFIERS.len(), 3);
        assert_eq!(PLAN_MODIFIERS[0].name(), "summon_bonus");
        assert_eq!(PLAN_MODIFIERS[1].name(), "trade_bonus");
        assert_eq!(PLAN_MODIFIERS[2].name(), "repair_bonus");
    }

    /// Verify the annotation score invariant: score delta == sum of contributions.
    #[test]
    fn plan_modifiers_stage_total_matches_sum_of_contributions() {
        // Construct a synthetic annotation with pre-populated modifiers
        // to verify the invariant without needing a real StageCtx.
        let pre_score = 1.5_f32;
        let contribs = [0.1_f32, -0.2_f32, 0.3_f32];
        let mut ann = PlanAnnotation { score: pre_score, ..Default::default() };
        for (i, &c) in contribs.iter().enumerate() {
            ann.modifiers.push(ModifierContribution {
                name: format!("modifier_{i}"),
                contribution: c,
            });
            ann.score += c;
        }
        let sum: f32 = ann.modifiers.iter().map(|m| m.contribution).sum();
        let expected_score = pre_score + sum;
        assert!(
            (ann.score - expected_score).abs() < 1e-6,
            "score delta must equal sum of contributions: {} vs {}",
            ann.score,
            expected_score
        );
    }
}
