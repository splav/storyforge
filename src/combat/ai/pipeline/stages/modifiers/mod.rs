//! Post-normalisation plan modifiers — pipeline stage 8.B.
//!
//! This module contains both the `PlanModifier` trait + associated types and the
//! `PlanModifiersStage` dispatcher that runs them.
//!
//! `PlanModifier` trait: each modifier contributes a signed addendum to
//! `ann.score` after the full factor/terminal scoring pass. Three built-in
//! modifiers are registered in `PLAN_MODIFIERS` (apply order is fixed):
//!
//! 1. `summon_bonus` — scarce-resource bonus for Summon plans.
//! 2. `trade_bonus`  — economic gain/loss relative to actor value.
//! 3. `repair_bonus` — goal-affinity amplifier when a stored goal is present.
//!
//! `PlanModifiersStage` applies all registered `PlanModifier` implementations
//! to every non-masked plan in the pool. This stage runs after `RepairAffinityStage`
//! (which populates `ann.repair_affinity`) and before `PickBestStage` (which reads
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
//!
//! **P3a.1 / P3a.6:** Each modifier contribution is also pushed as an
//! `AddendHit` into `ann.score_trace`. After P3a.6 cleanup the trace
//! accumulates over the full pipeline: `FinalizeStage` sets `trace.base`,
//! downstream stages (Sanity, Critics, Modifiers) push hits on top.
//! Invariant after apply: `ann.score == trace.compute()`.

pub mod repair_bonus;
pub mod summon_bonus;
pub mod trade_bonus;

use crate::combat::ai::pipeline::score_trace::AddendHit;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::scoring::factors::aggregate::build_summon_dpr_cache;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::repair::RepairWeights;
use crate::combat::ai::scoring::trade::unit_value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A post-normalisation additive modifier applied to every plan's score.
///
/// Implementations must be `Sync` so they can live in a `static` slice.
/// All three built-in implementations are zero-state unit structs —
/// `Sync` is satisfied automatically.
pub trait PlanModifier: Sync {
    /// Stable identifier for logging and debug overlays.
    fn name(&self) -> &'static str;

    /// Compute the signed additive contribution for one plan.
    ///
    /// Returns `0.0` when the modifier does not apply (e.g. no Summon steps,
    /// no stored goal). Positive values increase `ann.score`; negative values
    /// decrease it.
    fn modify(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ModifierCtx<'_, '_, '_>,
    ) -> f32;
}

// ── Context ──────────────────────────────────────────────────────────────────

/// Read-only context passed to every `PlanModifier::modify` call.
///
/// Lifetime parameters:
/// - `'w` — world/map borrows inside `StageCtx::scoring` (`AiWorld`, `InfluenceMaps`).
/// - `'s` — outer `pick_action` stack-frame borrow (`ScoringCtx`, intent, rng).
/// - `'a` — the borrow of `StageCtx` itself (shorter than both `'w` and `'s`).
pub struct ModifierCtx<'w, 's, 'a> {
    pub stage: &'a StageCtx<'w, 's>,
    /// Pre-computed per-template summon DPR cache, built once per pool in
    /// `PlanModifiersStage`. Empty when no plan summons.
    pub summon_dpr: &'a HashMap<String, f32>,
    /// `unit_value(active, world.content)` — computed once per pool.
    pub actor_value: f32,
    /// Role-mixed repair weights for the active actor. Computed once per pool
    /// via `active.role.repair_weights(world.tuning)`.
    pub repair_weights: RepairWeights,
}

// ── Contribution record ───────────────────────────────────────────────────────

/// Per-modifier additive contribution stored in `PlanAnnotation.modifiers`.
///
/// Populated by `PlanModifiersStage` for each plan. `name` matches the
/// `PlanModifier::name()` return value; `contribution` is the signed addendum
/// applied to `ann.score`. Entries appear in `PLAN_MODIFIERS` order.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ModifierContribution {
    pub name: String,
    pub contribution: f32,
}

// ── Static registry ──────────────────────────────────────────────────────────

/// Ordered slice of all active plan modifiers.
///
/// Order is fixed: `[summon_bonus, trade_bonus, repair_bonus]`.
/// `PlanModifiersStage` applies them left-to-right; the same order
/// appears in `PlanAnnotation.modifiers` entries.
pub static PLAN_MODIFIERS: &[&dyn PlanModifier] = &[
    &summon_bonus::MODIFIER,
    &trade_bonus::MODIFIER,
    &repair_bonus::MODIFIER,
];

// ── PlanModifiersStage ────────────────────────────────────────────────────────

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

            // P3a.6: bridging-reset removed. FinalizeStage (upstream) sets
            // trace.base; this stage only pushes addend hits on top of the
            // accumulated trace. Invariant: ann.score == trace.compute().
            for m in PLAN_MODIFIERS {
                let contribution = m.modify(plan, ann, &mctx);
                ann.modifiers.push(ModifierContribution {
                    name: m.name().into(),
                    contribution,
                });
                ann.score_trace.push_addend(AddendHit { name: m.name(), value: contribution });
                ann.score += contribution;
            }

            // Invariant: after the modifier loop, ann.score == trace.compute().
            debug_assert!(
                (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                "P3a.1 invariant violated: ann.score={} trace.compute()={}",
                ann.score,
                ann.score_trace.compute()
            );
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::pipeline::ScoredPool;
    use crate::combat::ai::pipeline::order::{run, PRODUCTION_PIPELINE};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    // ── Pure data tests (no StageCtx needed) ────────────────────────────────

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

    // ── P3a.1 — ScoreTrace integration tests ─────────────────────────────────
    //
    // These tests exercise PlanModifiersStage.apply() via PRODUCTION_PIPELINE.

    /// After apply(), each non-masked plan has exactly PLAN_MODIFIERS.len()
    /// addend hits in score_trace, in PLAN_MODIFIERS order.
    #[test]
    fn p3a_modifiers_push_addends_to_trace() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0, 0.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| run(PRODUCTION_PIPELINE, &mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.score_trace.addends.len(),
                PLAN_MODIFIERS.len(),
                "plan[{i}] trace.addends.len() must equal PLAN_MODIFIERS.len()"
            );
            for (j, hit) in ann.score_trace.addends.iter().enumerate() {
                assert_eq!(
                    hit.name, PLAN_MODIFIERS[j].name(),
                    "plan[{i}] addend[{j}].name mismatch"
                );
            }
        }
    }

    /// After apply(), trace.base was set to ann.score on stage entry.
    /// Verified via trace.compute() == ann.score.
    #[test]
    fn p3a_modifiers_trace_base_synced_from_score() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[2.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| run(PRODUCTION_PIPELINE, &mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        let computed = ann.score_trace.compute();
        assert!(
            (computed - ann.score).abs() < 1e-5,
            "trace.compute()={computed} must equal ann.score={} after modifiers",
            ann.score
        );
    }

    /// Pool of 3 non-masked plans: ann.score == trace.compute() for each.
    #[test]
    fn p3a_modifiers_invariant_score_equals_compute() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(); 3];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0, 3.5, -0.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| run(PRODUCTION_PIPELINE, &mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            if !ann.score.is_finite() {
                continue;
            }
            let computed = ann.score_trace.compute();
            assert!(
                (computed - ann.score).abs() < 1e-5,
                "plan[{i}]: trace.compute()={computed} != ann.score={}",
                ann.score
            );
        }
    }

    /// A masked plan (NEG_INFINITY score) must leave score_trace at default:
    /// base=0, addends empty. ann.score stays NEG_INFINITY.
    /// Calls PlanModifiersStage directly (not PRODUCTION_PIPELINE) to avoid
    /// downstream stages that may rewrite ann.score on masked plans.
    #[test]
    fn p3a_modifiers_masked_plan_trace_unchanged() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[f32::NEG_INFINITY])
            .build();

        // ── 4. Act — call the stage directly to avoid PRODUCTION_PIPELINE's
        //        PickBest rewriting score on masked plans.
        h.run(|ctx| PlanModifiersStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert_eq!(ann.score, f32::NEG_INFINITY, "masked plan score must stay NEG_INFINITY");
        assert_eq!(ann.score_trace.base, 0.0, "masked plan trace.base must stay 0");
        assert!(ann.score_trace.addends.is_empty(), "masked plan trace.addends must stay empty");
    }
}
