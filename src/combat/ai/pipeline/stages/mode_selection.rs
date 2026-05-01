//! ModeSelectionStage — step 11.0.
//!
//! Selects an
//! `EvaluationMode` for each plan via `select_evaluation_modes` and records
//! the decision in `ann.adaptation`.
//!
//! **Critically, this stage does NOT touch `ann.score` or `ann.factors`.**
//! Score mutation is deferred to `FinalizeStage`, which runs immediately
//! after this stage in the pipeline. This ordering ensures that
//! `SanityStage` and `CriticsStage`, which apply multiplicative modifiers
//! to `ann.score`, execute on the already-finalized base score and are
//! never overwritten.
//!
//! # Pipeline position (step 11.0)
//!
//! ```text
//! Viability → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask
//!          → KillableGate → RepairAffinity → PlanModifiers → PickBest
//! ```
//!
//! # `ann.adaptation.original_score` semantics
//!
//! The `original_score` field is set to `ann.score` at the time this stage
//! runs — which is the **Default-mode initial score** (post-Viability,
//! pre-Finalize). In prior pipeline order (step 7.2) this field captured
//! the post-Sanity/Critics score. The field is used only for debug/log
//! purposes so the semantic change is benign; it is documented here for
//! clarity.

use crate::combat::ai::outcome::AdaptationData;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::adapt::select_evaluation_modes;

pub struct ModeSelectionStage;

impl PlanStage for ModeSelectionStage {
    fn name(&self) -> &'static str {
        "mode_selection"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Collect read-only views for mode selection.
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let adaptation = select_evaluation_modes(
            &pool.plans,
            &raw_factors,
            &ctx.intent,
            ctx.scoring,
        );

        // Write adaptation annotations — do NOT touch ann.score or ann.factors.
        for (i, ann) in pool.annotations.iter_mut().enumerate() {
            if let Some(r) = adaptation.reasons.get(i).and_then(|r| r.as_ref()) {
                ann.adaptation = Some(AdaptationData {
                    reason: r.clone(),
                    // original_score here is the Default-mode initial score
                    // (post-Viability, pre-Finalize). See module doc for semantics.
                    original_score: ann.score,
                });
            }
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
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
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

    fn move_plan(dest: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![dest] }],
            final_pos: dest,
            ..TurnPlan::default()
        }
    }

    fn pfv_survival(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::SelfSurvival, v);
        f
    }

    /// Run ModeSelectionStage on a pool with given actor and return the pool.
    fn run_mode_selection(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<PlanFactorValues>,
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
        for (ann, (score, raw_f)) in pool
            .annotations
            .iter_mut()
            .zip(scores.into_iter().zip(raw.into_iter()))
        {
            ann.score = score;
            ann.factors = raw_f;
        }
        ModeSelectionStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── mode_selection_does_not_mutate_score ──────────────────────────────

    /// ModeSelectionStage must not change ann.score for any plan, including
    /// those that triggered LastStand mode.
    #[test]
    fn mode_selection_does_not_mutate_score() {
        // ProtectSelf with no defensive plan → ProtectSelfNoDefensive triggers.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        // One plan that would trigger LastStand (no survival), one Default-mode plan.
        let plans = vec![empty_plan(), empty_plan()];
        let pre_scores = vec![0.7_f32, 0.3_f32];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_mode_selection(
            plans,
            pre_scores.clone(),
            raw,
            &actor,
            &snap,
            TacticalIntent::ProtectSelf,
        );

        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.score, pre_scores[i],
                "ModeSelectionStage must not mutate ann.score[{}]",
                i
            );
        }
    }

    // ── mode_selection_writes_adaptation_for_laststand_plans ──────────────

    /// Plans that trigger LastStand must have ann.adaptation = Some(_) after
    /// ModeSelectionStage.
    #[test]
    fn mode_selection_writes_adaptation_for_laststand_plans() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        // ProtectSelf with no defensive options → all plans get LastStand.
        let plans = vec![empty_plan(), move_plan(hex_from_offset(1, 0))];
        let scores = vec![0.5, 0.4];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_mode_selection(
            plans,
            scores,
            raw,
            &actor,
            &snap,
            TacticalIntent::ProtectSelf,
        );

        for (i, ann) in pool.annotations.iter().enumerate() {
            assert!(
                ann.adaptation.is_some(),
                "plan[{}] should have adaptation annotation (ProtectSelfNoDefensive)",
                i
            );
        }
    }

    // ── mode_selection_records_original_score ────────────────────────────

    /// When adaptation triggers, ann.adaptation.original_score must equal
    /// the pre-adaptation ann.score.
    #[test]
    fn mode_selection_records_original_score() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![empty_plan(), empty_plan()];
        let pre_scores = vec![0.5_f32, 0.4_f32];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        let pool = run_mode_selection(
            plans,
            pre_scores.clone(),
            raw,
            &actor,
            &snap,
            TacticalIntent::ProtectSelf,
        );

        for (i, ann) in pool.annotations.iter().enumerate() {
            let data = ann.adaptation.as_ref().expect("expected adaptation");
            assert_eq!(
                data.original_score, pre_scores[i],
                "original_score[{}] should match pre-adaptation score",
                i
            );
        }
    }

    // ── mode_selection_adaptation_reason_round_trips_to_intent ────────────

    /// IntentReason::Adapted built from ann.adaptation encodes the correct
    /// AdaptationReason (parity with legacy adaptation_data_round_trips_through_intent_reason).
    #[test]
    fn mode_selection_adaptation_reason_round_trips_to_intent() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![empty_plan()];
        let scores = vec![0.5_f32];
        let raw = vec![pfv_survival(0.0)];

        let pool = run_mode_selection(
            plans,
            scores,
            raw,
            &actor,
            &snap,
            TacticalIntent::ProtectSelf,
        );

        let adapt = pool.annotations[0]
            .adaptation
            .as_ref()
            .expect("expected adaptation");
        let prior = IntentReason::NoRuleDefault;
        let wrapped = IntentReason::Adapted {
            prior: Box::new(prior),
            reason: adapt.reason.clone(),
        };

        match wrapped {
            IntentReason::Adapted { reason, .. } => {
                assert!(
                    matches!(
                        reason,
                        crate::combat::ai::adapt::AdaptationReason::ProtectSelfNoDefensive
                    ),
                    "expected ProtectSelfNoDefensive, got {:?}",
                    reason,
                );
            }
            _ => panic!("expected Adapted variant"),
        }
    }

    // ── mode_selection_no_adaptation_when_no_trigger ──────────────────────

    /// Healthy actor with no AoO threats under non-ProtectSelf intent →
    /// no adaptation annotation written.
    #[test]
    fn mode_selection_no_adaptation_when_no_trigger() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(20).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![move_plan(hex_from_offset(1, 0))];
        let scores = vec![0.5];
        let raw = vec![PlanFactorValues::default()];

        let pool = run_mode_selection(
            plans,
            scores,
            raw,
            &actor,
            &snap,
            TacticalIntent::Reposition,
        );

        assert!(
            pool.annotations[0].adaptation.is_none(),
            "expected no adaptation when no trigger fires"
        );
        // Score must also be unchanged.
        assert_eq!(pool.annotations[0].score, 0.5);
    }
}
