//! ProtectSelfMaskStage — step 7.2.
//!
//! Replicates `PlanRanking::apply_protect_self` as a `PlanStage` with an
//! **internal predicate**: the stage skips entirely when `ctx.intent` is not
//! `ProtectSelf`. This removes the `if matches!(ranking.intent, ProtectSelf)`
//! guard from `pick_action` body.
//!
//! Writes `annotation.contract = Some(ContractMaskHit { mask: "protect_self", … })`
//! for every plan whose score is set to -∞ by the mask.

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::outcome::ContractMaskHit;
use crate::combat::ai::pipeline::score_trace::{MaskHit, MaskKind};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::apply_protect_self_mask;

pub struct ProtectSelfMaskStage;

impl PlanStage for ProtectSelfMaskStage {
    fn name(&self) -> &'static str {
        "protect_self_mask"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Internal predicate — only active under ProtectSelf intent.
        if !matches!(ctx.intent, TacticalIntent::ProtectSelf) {
            return;
        }

        let epsilon = ctx.scoring.world.tuning.thresholds.self_survival_epsilon;
        // Snapshot scores before masking.
        let pre_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Collect adaptation modes from annotations (needed by apply_protect_self_mask).
        let modes: Vec<_> = pool
            .annotations
            .iter()
            .map(|ann| {
                ann.adaptation
                    .as_ref()
                    .map(|_| crate::combat::ai::adapt::EvaluationMode::LastStand)
                    .unwrap_or(crate::combat::ai::adapt::EvaluationMode::Default)
            })
            .collect();

        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
        let mut scores: Vec<f32> = pre_scores.clone();
        apply_protect_self_mask(&mut scores, &raw_factors, &modes, epsilon);

        // Write back updated scores and contract annotations.
        for (i, (ann, new_score)) in pool.annotations.iter_mut().zip(scores.into_iter()).enumerate() {
            if new_score == f32::NEG_INFINITY && pre_scores[i].is_finite() {
                ann.contract = Some(ContractMaskHit {
                    mask: "protect_self".into(),
                    original_score: pre_scores[i],
                });

                // P3a.4 / P3a.6: push MaskHit on the accumulated trace.
                // FinalizeStage (upstream) already set trace.base; bridging-reset removed.
                ann.score_trace.push_mask(MaskHit {
                    kind: MaskKind::Poison,
                    source: "protect_self",
                });

                // Invariant: for masked plans, ann.score == compute() == NEG_INFINITY.
                debug_assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY);
            }
            ann.score = new_score;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn pfv_survival(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::SelfSurvival, v);
        f
    }

    fn run_stage(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<PlanFactorValues>,
        intent: TacticalIntent,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            intent,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, (score, raw_f)) in pool.annotations.iter_mut().zip(scores.into_iter().zip(raw.into_iter())) {
            ann.score = score;
            ann.factors = raw_f;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            if score.is_finite() {
                ann.score_trace.base = score;
            }
        }
        ProtectSelfMaskStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── internal predicate ────────────────────────────────────────────────────

    #[test]
    fn protect_self_mask_skips_when_intent_not_protect_self() {
        // Reposition intent → stage is a no-op; no annotation, no score change.
        let plans = vec![TurnPlan::default()];
        let scores = vec![0.5_f32];
        let raw = vec![pfv_survival(0.0)];

        let pool = run_stage(plans, scores, raw, TacticalIntent::Reposition);

        // score unchanged
        assert_eq!(pool.annotations[0].score, 0.5, "score should be untouched for non-ProtectSelf intent");
        assert!(pool.annotations[0].contract.is_none(), "no contract annotation expected");
    }

    // ── mask writes contract annotation ───────────────────────────────────────

    #[test]
    fn protect_self_mask_writes_contract_when_non_defensive() {
        // Two plans: one defensive (self_survival ≥ epsilon=0.01), one not.
        // The non-defensive plan should be masked to -∞ and get the annotation.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let scores = vec![0.5_f32, 0.7_f32];
        let raw = vec![
            pfv_survival(0.5), // defensive
            pfv_survival(0.0), // non-defensive
        ];

        let pool = run_stage(plans, scores, raw, TacticalIntent::ProtectSelf);

        // plan 0: defensive → score unchanged, no annotation
        assert!(pool.annotations[0].score.is_finite(), "defensive plan should not be masked");
        assert!(pool.annotations[0].contract.is_none(), "no contract annotation for defensive plan");

        // plan 1: non-defensive → masked + annotation
        assert_eq!(pool.annotations[1].score, f32::NEG_INFINITY, "non-defensive plan should be masked");
        let contract = pool.annotations[1].contract.as_ref()
            .expect("expected contract annotation for non-defensive plan");
        assert_eq!(contract.mask, "protect_self".to_string());
        assert_eq!(contract.original_score, 0.7_f32);
    }

    #[test]
    fn protect_self_mask_no_annotation_when_all_defensive() {
        // All plans are defensive — mask is a no-op for scores, no annotations written.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let scores = vec![0.5_f32, 0.4_f32];
        let raw = vec![pfv_survival(0.5), pfv_survival(0.3)];

        let pool = run_stage(plans, scores, raw, TacticalIntent::ProtectSelf);

        for ann in &pool.annotations {
            assert!(ann.contract.is_none(), "no contract annotation when all plans are defensive");
        }
    }

    // ── P3a.4: ScoreTrace emission ────────────────────────────────────────────

    #[test]
    fn p3a_protect_self_mask_emits_mask_hit() {
        // Non-defensive plan under ProtectSelf intent → MaskHit Poison emitted.
        let plans = vec![TurnPlan::default()];
        let scores = vec![0.5_f32];
        let raw = vec![pfv_survival(0.0)]; // survival=0.0 → non-defensive

        let pool = run_stage(plans, scores, raw, TacticalIntent::ProtectSelf);

        let trace = &pool.annotations[0].score_trace;
        assert_eq!(trace.masks.len(), 1, "exactly one MaskHit expected");
        assert_eq!(trace.masks[0].kind, crate::combat::ai::pipeline::score_trace::MaskKind::Poison);
        assert_eq!(trace.masks[0].source, "protect_self");
        assert!(trace.gates.is_empty(), "no GateHit expected for ProtectSelf mask");
    }

    #[test]
    fn p3a_protect_self_mask_no_hit_when_defensive() {
        // Defensive plan (survival ≥ epsilon) → score unchanged, trace.masks empty.
        let plans = vec![TurnPlan::default()];
        let scores = vec![0.5_f32];
        let raw = vec![pfv_survival(0.5)]; // survival=0.5 → defensive

        let pool = run_stage(plans, scores, raw, TacticalIntent::ProtectSelf);

        let ann = &pool.annotations[0];
        assert!(ann.score.is_finite() && (ann.score - 0.5).abs() < 1e-6,
            "defensive plan score should be unchanged: got {}", ann.score);
        assert!(ann.score_trace.masks.is_empty(), "no MaskHit expected for defensive plan");
    }

    #[test]
    fn p3a_protect_self_mask_invariant() {
        // Masked plan: ann.score == NEG_INFINITY, trace.compute() == NEG_INFINITY.
        let plans = vec![TurnPlan::default()];
        let scores = vec![0.7_f32];
        let raw = vec![pfv_survival(0.0)]; // non-defensive → will be masked

        let pool = run_stage(plans, scores, raw, TacticalIntent::ProtectSelf);

        let ann = &pool.annotations[0];
        assert_eq!(ann.score, f32::NEG_INFINITY, "masked plan score must be NEG_INFINITY");
        assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY,
            "trace.compute() must equal NEG_INFINITY for masked plan");
    }
}
