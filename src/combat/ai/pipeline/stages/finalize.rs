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
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::planning::rescore_with_per_plan_modes;

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

        // Write back scores and updated factors.
        for (ann, (new_score, new_raw)) in pool
            .annotations
            .iter_mut()
            .zip(new_scores.into_iter().zip(raw_factors.into_iter()))
        {
            ann.score = new_score;
            ann.factors = new_raw;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::PlanFactorValues;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::outcome::AdaptationData;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::adapt::AdaptationReason;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::planning::score_plans_with_raw;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

    fn run_finalize(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<PlanFactorValues>,
        adaptations: Vec<Option<AdaptationData>>,
        actor: &crate::combat::ai::world::snapshot::UnitSnapshot,
        snap: &BattleSnapshot,
        intent: TacticalIntent,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, snap, &maps, &reservations, actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            intent,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, ((score, raw_f), adaptation)) in pool
            .annotations
            .iter_mut()
            .zip(scores.into_iter().zip(raw.into_iter()).zip(adaptations.into_iter()))
        {
            ann.score = score;
            ann.factors = raw_f;
            ann.adaptation = adaptation;
        }
        FinalizeStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── finalize_applies_per_plan_modes ────────────────────────────────────

    /// A plan with adaptation = Some (LastStand mode) must produce a score
    /// that is recomputed from raw factors (annotation intact after finalize).
    /// FinalizeStage must not clear the adaptation annotation written by
    /// ModeSelectionStage.
    #[test]
    fn finalize_applies_per_plan_modes() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);

        let plans = vec![empty_plan()];
        let scores = vec![0.5_f32];
        let raw = vec![PlanFactorValues::default()];

        // Inject a LastStand adaptation annotation.
        let adaptation = Some(AdaptationData {
            reason: AdaptationReason::ProtectSelfNoDefensive,
            original_score: 0.5,
        });

        let pool = run_finalize(
            plans,
            scores,
            raw,
            vec![adaptation],
            &actor,
            &snap,
            TacticalIntent::Reposition,
        );

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
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(20).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);

        // Compute the initial Default-mode score.
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let intent = TacticalIntent::Reposition;
        let mut plans = vec![empty_plan()];
        let (initial_scores, initial_raw) =
            score_plans_with_raw(&mut plans, &intent, &scoring);
        let initial_score = initial_scores[0];

        // Run FinalizeStage with that same initial score/factors, no adaptation.
        let pool = run_finalize(
            vec![empty_plan()],
            vec![initial_score],
            vec![initial_raw[0]],
            vec![None],
            &actor,
            &snap,
            intent,
        );

        let after_score = pool.annotations[0].score;
        assert!(
            (after_score - initial_score).abs() < 1e-5,
            "Default-mode finalize should be idempotent: initial={initial_score}, after={after_score}"
        );
    }
}
