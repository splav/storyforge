//! FinalizeStage — step 11.0.
//!
//! Works in tandem with `ModeSelectionStage` (the mode-selection half, B3 fix). Applies
//! mode-aware score finalization: reads `ann.adaptation` for each plan
//! to determine its `EvaluationMode`, then calls `rescore_with_per_plan_modes`
//! to rewrite `ann.score` and `ann.factors` from raw intent/tempo factors.
//!
//! This is a **replacement** finalization pass, not a multiplicative one.
//! After this stage runs, Sanity and Critics apply their multipliers on top
//! — and nothing downstream overwrites the score again (B3 fix).
//!
//! # Pipeline position (step 11.0)
//!
//! ```text
//! Viability → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask
//!          → KillableGate → RepairAffinity → PlanModifiers → PickBest
//! ```
//!
//! # Behaviour
//!
//! - For plans with `ann.adaptation = None` (mode = Default): the rescore
//!   uses the same global intent as the initial pass → **idempotent** (same
//!   factor weights, same score). Slight redundant compute, zero behavior change.
//! - For plans with `ann.adaptation = Some(_)` (mode = LastStand): the
//!   rescore applies the LastStand intent column → score diverges from the
//!   initial Default-mode score. This is the fix: Sanity/Critics then
//!   multiply on the correct base.

use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::pipeline::score_trace::ScoreTrace;
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::scoring::factors::aggregate::rescore_with_per_plan_modes;

pub struct FinalizeStage;

impl PlanStage for FinalizeStage {
    fn name(&self) -> &'static str {
        "finalize"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        if pool.is_empty() {
            return;
        }

        // Derive per-plan EvaluationMode from adaptation annotations.
        let modes: Vec<EvaluationMode> = pool
            .annotations
            .iter()
            .map(|ann| {
                if ann.adaptation.is_some() {
                    EvaluationMode::LastStand
                } else {
                    EvaluationMode::Default
                }
            })
            .collect();

        // Extract mutable raw_factors.
        let mut raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        // Recompute intent + tempo columns and produce fresh scores.
        let new_scores = rescore_with_per_plan_modes(
            &mut pool.plans,
            &mut raw_factors,
            &modes,
            &ctx.intent,
            ctx.scoring,
        );

        // Write back scores, updated factors, and trace (P3a.5: Rescore semantics).
        for (i, (ann, (new_score, new_raw))) in pool
            .annotations
            .iter_mut()
            .zip(new_scores.into_iter().zip(raw_factors.into_iter()))
            .enumerate()
        {
            ann.score = new_score;
            ann.factors = new_raw;

            // P3a.5: Rescore semantics — base = new_score, rescore_mode = mode, effects cleared.
            // FinalizeStage is the ONLY stage with ScoreEffect::Rescore (see STAGE_SPECS).
            // Any upstream effects accumulated in trace before Finalize are considered stale.
            ann.score_trace = ScoreTrace {
                base: new_score,
                rescore_mode: Some(modes[i]),
                ..Default::default()
            };

            // Invariant: ann.score == trace.compute() (base only, no effects).
            if new_score.is_finite() {
                debug_assert!(
                    (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                    "P3a.5 invariant violated: plan[{i}] ann.score={} vs compute()={}",
                    ann.score,
                    ann.score_trace.compute(),
                );
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::adapt::AdaptationReason;
    use crate::combat::ai::outcome::AdaptationData;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::scoring::factors::{aggregate::score_plans_with_raw, PlanFactorValues};
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, PoolBuilder,
        StageTestHarness, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

    // ── finalize_applies_per_plan_modes ────────────────────────────────────

    /// A plan with adaptation = Some (LastStand mode) must produce a score
    /// that is recomputed from raw factors (annotation intact after finalize).
    /// FinalizeStage must not clear the adaptation annotation written by
    /// ModeSelectionStage.
    #[test]
    fn finalize_applies_per_plan_modes() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![empty_plan()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let adaptation = Some(AdaptationData {
            reason: AdaptationReason::ProtectSelfNoDefensive,
            original_score: 0.5,
        });
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![PlanFactorValues::default()])
            .adaptations(vec![adaptation])
            .build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        // FinalizeStage rewrites ann.score from raw factors; the key invariant
        // here is that the adaptation annotation is NOT cleared (it is written
        // by ModeSelectionStage and consumed later for debug logging).
        assert!(
            pool.annotations[0].adaptation.is_some(),
            "FinalizeStage must not clear adaptation annotation"
        );
    }

    // ── finalize_default_mode_idempotent ───────────────────────────────────

    /// For a plan with no adaptation (mode = Default), FinalizeStage
    /// should produce a score equal to the initial score (same intent,
    /// same factors → same result from finalize_scores).
    #[test]
    fn finalize_default_mode_idempotent() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).full_hp(20).build();
        // Compute the initial Default-mode score using the same context as
        // the harness builds internally.
        let snap = crate::combat::ai::world::snapshot::BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = crate::combat::ai::world::reservations::Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let intent = crate::combat::ai::intent::TacticalIntent::Reposition;
        let mut plans = vec![empty_plan()];
        let (initial_scores, initial_raw) =
            score_plans_with_raw(&mut plans, &intent, &scoring);
        let initial_score = initial_scores[0];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(vec![empty_plan()])
            .scores(&[initial_score])
            .factors(vec![initial_raw[0]])
            .build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let after_score = pool.annotations[0].score;
        assert!(
            (after_score - initial_score).abs() < 1e-5,
            "Default-mode finalize should be idempotent: initial={initial_score}, after={after_score}"
        );
    }

    // ── P3a.5 tests ────────────────────────────────────────────────────────────

    /// After FinalizeStage, trace.base == ann.score and rescore_mode = Default.
    /// All effect vecs must be empty (only base is set).
    #[test]
    fn p3a_finalize_sets_trace_base_to_new_score() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let plans = vec![empty_plan()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![PlanFactorValues::default()])
            .build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert!(
            (ann.score - ann.score_trace.base).abs() < 1e-5,
            "trace.base must equal ann.score: score={}, base={}",
            ann.score,
            ann.score_trace.base,
        );
        assert_eq!(ann.score_trace.rescore_mode, Some(EvaluationMode::Default));
        assert!(ann.score_trace.multipliers.is_empty());
        assert!(ann.score_trace.addends.is_empty());
        assert!(ann.score_trace.masks.is_empty());
        assert!(ann.score_trace.gates.is_empty());
        assert!(
            (ann.score_trace.compute() - ann.score).abs() < 1e-5,
            "compute() must equal ann.score: compute={}, score={}",
            ann.score_trace.compute(),
            ann.score,
        );
    }

    /// Upstream trace effects set BEFORE FinalizeStage must be cleared after it runs.
    /// base is overwritten with new_score, not preserved from the stale upstream value.
    #[test]
    fn p3a_finalize_clears_upstream_effects() {
        use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind};

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let plans = vec![empty_plan()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool — inject stale upstream trace via customize() ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.8])
            .factors(vec![PlanFactorValues::default()])
            .customize(|anns| {
                anns[0].score_trace = crate::combat::ai::pipeline::score_trace::ScoreTrace {
                    base: 999.0,
                    ..Default::default()
                };
                anns[0].score_trace.push_multiplier(MultiplierHit {
                    kind: MultiplierKind::Sanity,
                    value: 0.5,
                });
            })
            .build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        // Old base (999.0) must be gone; base = new_score from rescore.
        assert!(
            (ann.score_trace.base - ann.score).abs() < 1e-5,
            "base must be new_score, not stale 999.0: base={}, score={}",
            ann.score_trace.base,
            ann.score,
        );
        assert!(ann.score_trace.multipliers.is_empty(), "upstream multipliers must be cleared");
        assert!((ann.score_trace.compute() - ann.score).abs() < 1e-5);
    }

    /// Two plans — one with LastStand adaptation, one without.
    /// rescore_mode must reflect the per-plan mode.
    #[test]
    fn p3a_finalize_records_rescore_mode_per_plan() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(5).max_hp(20).build();
        let plans = vec![empty_plan(), empty_plan()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let adaptation_last_stand = Some(AdaptationData {
            reason: AdaptationReason::ProtectSelfNoDefensive,
            original_score: 0.5,
        });
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.8])
            .factors(vec![PlanFactorValues::default(), PlanFactorValues::default()])
            .adaptations(vec![adaptation_last_stand, None])
            .build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        assert_eq!(
            pool.annotations[0].score_trace.rescore_mode,
            Some(EvaluationMode::LastStand),
            "plan with adaptation must have LastStand rescore_mode"
        );
        assert_eq!(
            pool.annotations[1].score_trace.rescore_mode,
            Some(EvaluationMode::Default),
            "plan without adaptation must have Default rescore_mode"
        );
    }

    /// Empty pool: FinalizeStage must return immediately without panicking.
    #[test]
    fn p3a_finalize_empty_pool_no_op() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(vec![]).build();

        // ── 4. Act ──
        h.run(|ctx| FinalizeStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        assert!(pool.is_empty(), "pool must remain empty");
    }
}
