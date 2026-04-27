//! AdaptationStage — step 7.2.
//!
//! Replicates `PlanRanking::apply_adaptation` as a `PlanStage`. Calls
//! `planning::apply_adaptation` on the pool and writes per-plan
//! `annotation.adaptation` with the reason + original score for each plan
//! whose evaluation mode was switched away from Default.

use crate::combat::ai::outcome::AdaptationData;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::apply_adaptation;

pub struct AdaptationStage;

impl PlanStage for AdaptationStage {
    fn name(&self) -> &'static str {
        "adaptation"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Snapshot pre-adaptation scores.
        let pre_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Extract scores and raw_factors for apply_adaptation.
        let mut scores: Vec<f32> = pre_scores.clone();
        let mut raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let adaptation = apply_adaptation(
            &mut pool.plans,
            &mut raw_factors,
            &mut scores,
            &ctx.intent,
            ctx.scoring,
        );

        // Write back updated scores and raw_factors, and adaptation annotations.
        for (i, (ann, (new_score, new_raw))) in pool
            .annotations
            .iter_mut()
            .zip(scores.into_iter().zip(raw_factors.into_iter()))
            .enumerate()
        {
            ann.score = new_score;
            ann.factors = new_raw;
            if let Some(r) = adaptation.reasons.get(i).and_then(|r| r.as_ref()) {
                ann.adaptation = Some(AdaptationData {
                    reason: r.clone(),
                    original_score: pre_scores[i],
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    /// Minimal plan with a move path (for AoO exposure) — no steps → no AoO.
    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

    fn move_plan(dest: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![dest] }],
            final_pos: dest,
            ..TurnPlan::default()
        }
    }

    /// Run AdaptationStage on a pool with the given actor and return the pool.
    fn pfv_survival(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::SelfSurvival, v);
        f
    }

    fn run_adaptation(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<PlanFactorValues>,
        actor: &crate::combat::ai::snapshot::UnitSnapshot,
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
        for (ann, (score, raw_f)) in pool.annotations.iter_mut().zip(scores.into_iter().zip(raw.into_iter())) {
            ann.score = score;
            ann.factors = raw_f;
        }
        AdaptationStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── adaptation triggers → annotation populated ─────────────────────────

    #[test]
    fn adaptation_stage_writes_annotation_when_triggered() {
        // ProtectSelf with no defensive plan → ProtectSelfNoDefensive on all plans.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        // Two plans, neither defensive (self_survival=0.0 < epsilon).
        let plans = vec![empty_plan(), empty_plan()];
        let scores = vec![0.5, 0.4];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_adaptation(plans, scores, raw, &actor, &snap, TacticalIntent::ProtectSelf);

        for ann in &pool.annotations {
            assert!(
                ann.adaptation.is_some(),
                "expected adaptation annotation for ProtectSelfNoDefensive",
            );
        }
    }

    #[test]
    fn adaptation_stage_records_original_score() {
        // Same setup as above; original_score must equal the pre-adaptation value.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![empty_plan(), empty_plan()];
        let pre_scores = vec![0.5_f32, 0.4_f32];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_adaptation(
            plans, pre_scores.clone(), raw, &actor, &snap, TacticalIntent::ProtectSelf,
        );

        for (i, ann) in pool.annotations.iter().enumerate() {
            let data = ann.adaptation.as_ref().expect("expected adaptation");
            assert_eq!(
                data.original_score, pre_scores[i],
                "original_score[{}] should match pre-adaptation score", i,
            );
        }
    }

    #[test]
    fn adaptation_stage_skips_when_no_trigger_fires() {
        // Healthy actor, Reposition intent, no AoO threats → no adaptation.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(20).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![move_plan(hex_from_offset(1, 0))];
        let scores = vec![0.5];
        let raw = vec![PlanFactorValues::default()];

        let pool = run_adaptation(
            plans, scores, raw, &actor, &snap, TacticalIntent::Reposition,
        );

        assert!(
            pool.annotations[0].adaptation.is_none(),
            "expected no adaptation annotation when no trigger fires",
        );
    }

    #[test]
    fn adaptation_data_round_trips_through_intent_reason() {
        // Verify that building IntentReason::Adapted from the annotation
        // produces a reason with the same AdaptationReason.
        use crate::combat::ai::intent::IntentReason;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![empty_plan(), empty_plan()];
        let scores = vec![0.5, 0.4];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_adaptation(plans, scores, raw, &actor, &snap, TacticalIntent::ProtectSelf);

        let adapt = pool.annotations[0].adaptation.as_ref().expect("expected adaptation");
        let prior = IntentReason::NoRuleDefault;
        let wrapped = IntentReason::Adapted {
            prior: Box::new(prior),
            reason: adapt.reason.clone(),
        };

        // The wrapped reason encodes ProtectSelfNoDefensive.
        match wrapped {
            IntentReason::Adapted { reason, .. } => {
                assert!(
                    matches!(reason, crate::combat::ai::planning::AdaptationReason::ProtectSelfNoDefensive),
                    "expected ProtectSelfNoDefensive, got {:?}", reason,
                );
            }
            _ => panic!("expected Adapted variant"),
        }
    }
}
