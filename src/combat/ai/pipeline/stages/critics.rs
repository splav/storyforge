//! CriticsStage — step 10.0.
//!
//! Dispatches each plan through a registered `Vec<Box<dyn PlanCritic>>`.
//! For each critic hit, applies `ann.score *= hit.multiplier` and pushes the
//! hit into `ann.critics` for structured logging.
//!
//! `CriticsStage::first_wave()` registers all critics from steps 10.1–10.3.

use crate::combat::ai::critics::blindspot_ranged::BlindspotRanged;
use crate::combat::ai::critics::buff_into_void::BuffIntoVoid;
use crate::combat::ai::critics::heal_without_rescue_value::HealWithoutRescueValue;
use crate::combat::ai::critics::overcommit_into_danger::OvercommitIntoDanger;
use crate::combat::ai::critics::rare_resource_for_low_impact::RareResourceForLowImpact;
use crate::combat::ai::critics::self_lethal_without_payoff::SelfLethalWithoutPayoff;
use crate::combat::ai::critics::PlanCritic;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

// ── CriticsStage ──────────────────────────────────────────────────────────────

pub struct CriticsStage {
    critics: Vec<Box<dyn PlanCritic>>,
}

impl CriticsStage {
    /// Build the first-wave critic set.
    ///
    /// Step 10.1: defensive cluster — OvercommitIntoDanger + SelfLethalWithoutPayoff.
    /// Step 10.2: positioning cluster — BlindspotRanged.
    /// Step 10.3: resource/value cluster — BuffIntoVoid + RareResourceForLowImpact
    ///            + HealWithoutRescueValue.
    pub fn first_wave() -> Self {
        Self {
            critics: vec![
                Box::new(OvercommitIntoDanger),
                Box::new(SelfLethalWithoutPayoff),
                Box::new(BlindspotRanged),
                Box::new(BuffIntoVoid),
                Box::new(RareResourceForLowImpact),
                Box::new(HealWithoutRescueValue),
            ],
        }
    }
}

impl PlanStage for CriticsStage {
    fn name(&self) -> &'static str {
        "critics"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind, ScoreTrace};

        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            // P3a.2 partial-migration bridging: fully reset trace and treat
            // current ann.score as the new base, discarding any upstream trace
            // state. Cleaned up in P3a.6 once all stages are migrated.
            let entry_score = ann.score;
            ann.score_trace = ScoreTrace { base: entry_score, ..Default::default() };

            let mut applied_count = 0;

            for c in &self.critics {
                if let Some(hit) = c.evaluate(plan, ann, ctx.scoring) {
                    ann.score *= hit.multiplier;
                    ann.score_trace.push_multiplier(MultiplierHit {
                        kind: MultiplierKind::Critic,
                        value: hit.multiplier,
                    });
                    ann.critics.push(hit);
                    applied_count += 1;
                }
            }

            // Invariant: ann.score == trace.compute() after this stage.
            // Only checked for finite entry scores to avoid NaN corner cases
            // (e.g. NEG_INFINITY × 0.0 = NaN in some formulations).
            if applied_count > 0 && entry_score.is_finite() {
                debug_assert!(
                    (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                    "P3a.2 invariant violated: ann.score={} vs compute()={}",
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
    use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::combat::ai::utility::ScoringCtx;
    use crate::core::DiceRng;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn make_pool_with_scores(scores: Vec<f32>) -> ScoredPool {
        let plans: Vec<TurnPlan> = scores.iter().map(|_| TurnPlan::default()).collect();
        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip(scores.into_iter()) {
            ann.score = score;
        }
        pool
    }

    fn make_stage_ctx<'w, 's>(
        scoring: &'s crate::combat::ai::utility::ScoringCtx<'w, 's>,
        rng: &'s mut DiceRng,
        pos: crate::game::hex::Hex,
    ) -> StageCtx<'w, 's> {
        StageCtx::new(scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, rng)
    }

    fn apply_critics_to_pool(
        stage: CriticsStage,
        mut pool: ScoredPool,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = make_stage_ctx(&scoring, &mut rng, pos);
        stage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── empty critics vec — no-op ──────────────────────────────────────────

    #[test]
    fn critics_stage_no_op_when_empty() {
        // CriticsStage::first_wave() has no critics — scores and annotations
        // must be unchanged after apply().
        let stage = CriticsStage::first_wave();
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![0.8, 0.5]));

        assert_eq!(pool.annotations[0].score, 0.8, "score must not change with empty critics");
        assert_eq!(pool.annotations[1].score, 0.5, "score must not change with empty critics");
        for ann in &pool.annotations {
            assert!(
                ann.critics.is_empty(),
                "no critic hits expected for empty stage, got {:?}", ann.critics,
            );
        }
    }

    // ── mock critic fires and multiplies score ─────────────────────────────

    struct AlwaysHitCritic {
        multiplier: f32,
    }

    impl PlanCritic for AlwaysHitCritic {
        fn name(&self) -> &'static str {
            "always_hit"
        }

        fn evaluate(
            &self,
            _plan: &TurnPlan,
            _ann: &PlanAnnotation,
            _ctx: &ScoringCtx,
        ) -> Option<CriticHit> {
            use crate::combat::ai::critics::overcommit_into_danger::OvercommitSource;
            Some(CriticHit {
                critic: CriticKind::OvercommitIntoDanger,
                multiplier: self.multiplier,
                reason: CriticReason::OvercommitIntoDanger {
                    source: OvercommitSource::SurvivalPath,
                    ratio: 0.5,
                },
            })
        }
    }

    #[test]
    fn critics_stage_writes_hit_and_multiplies_score() {
        // A mock critic that always fires with multiplier 0.5.
        // After apply: score halved, ann.critics has exactly one entry.
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![1.0]));

        let ann = &pool.annotations[0];
        assert!(
            (ann.score - 0.5).abs() < 1e-6,
            "expected score 0.5 after 0.5× multiplier, got {}", ann.score,
        );
        assert_eq!(
            ann.critics.len(), 1,
            "expected exactly one critic hit, got {:?}", ann.critics,
        );
        assert_eq!(ann.critics[0].critic, CriticKind::OvercommitIntoDanger);
    }

    // ── critics_survive_through_adaptation_path (B3 regression) ──────────
    //
    // Regression test for B3 fix (step 11.0): in the old pipeline order
    // Critics ran before FinalizeStage, which would rescore ann.score from
    // raw factors — wiping the Critics multiplier. In the new order:
    //   ModeSelection → Finalize → Sanity → Critics → ...
    // Critics run AFTER Finalize, so their multipliers survive.
    //
    // This test runs a partial pipeline:
    //   ModeSelectionStage → FinalizeStage → AlwaysHitCritic(0.5)
    // and verifies that the final score = finalized_score × 0.5.

    #[allow(clippy::too_many_arguments)]
    fn run_partial_pipeline_with_critic(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<crate::combat::ai::factors::PlanFactorValues>,
        adaptations: Vec<Option<crate::combat::ai::outcome::AdaptationData>>,
        actor: &crate::combat::ai::world::snapshot::UnitSnapshot,
        snap: &BattleSnapshot,
        intent: TacticalIntent,
        critic_multiplier: f32,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, snap, &maps, &reservations, actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(&scoring, intent, IntentReason::NoRuleDefault, actor.pos, &mut rng);

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

        // Run partial pipeline: ModeSelection already ran (adaptation is pre-injected),
        // so we start from FinalizeStage then Critics.
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        FinalizeStage.apply(&mut pool, &mut ctx);

        let score_after_finalize: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Apply mock critic.
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: critic_multiplier })],
        };
        stage.apply(&mut pool, &mut ctx);

        // Verify: each score == finalized_score × multiplier.
        for (i, ann) in pool.annotations.iter().enumerate() {
            let expected = score_after_finalize[i] * critic_multiplier;
            assert!(
                (ann.score - expected).abs() < 1e-5,
                "plan[{i}]: critics multiplier was lost — expected {expected}, got {}",
                ann.score,
            );
        }

        pool
    }

    #[test]
    fn critics_survive_through_adaptation_path() {
        use crate::combat::ai::factors::PlanFactorValues;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::planning::types::TurnPlan;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);

        // Two plans: one with LastStand adaptation, one with Default (no adaptation).
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let scores = vec![0.8_f32, 0.6_f32];
        let raw = vec![PlanFactorValues::default(), PlanFactorValues::default()];
        let adaptations = vec![
            Some(AdaptationData {
                reason: AdaptationReason::ProtectSelfNoDefensive,
                original_score: 0.8,
            }),
            None, // Default mode
        ];

        // Both plans must have critics multiplier applied after Finalize.
        run_partial_pipeline_with_critic(
            plans,
            scores,
            raw,
            adaptations,
            &actor,
            &snap,
            TacticalIntent::Reposition,
            0.5,
        );
        // Assertions are inside run_partial_pipeline_with_critic.
    }

    // ── P3a.2 — ScoreTrace integration tests ─────────────────────────────────

    /// Mock critic fires with multiplier 0.5; trace must contain exactly one
    /// MultiplierHit with kind=Critic and value=0.5 after apply().
    #[test]
    fn p3a_critics_push_multipliers_to_trace() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;

        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![1.0]));

        let trace = &pool.annotations[0].score_trace;
        assert_eq!(trace.multipliers.len(), 1, "expected exactly one multiplier hit");
        assert_eq!(trace.multipliers[0].kind, MultiplierKind::Critic);
        assert!(
            (trace.multipliers[0].value - 0.5).abs() < 1e-6,
            "multiplier value must be 0.5, got {}",
            trace.multipliers[0].value
        );
    }

    /// entry_score=1.0, multiplier=0.5: trace.base == 1.0 (synced from entry
    /// score) and trace.compute() == 0.5 == ann.score.
    #[test]
    fn p3a_critics_trace_base_synced_from_score() {
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![1.0]));

        let ann = &pool.annotations[0];
        assert!(
            (ann.score_trace.base - 1.0).abs() < 1e-6,
            "trace.base must be synced to entry score 1.0, got {}",
            ann.score_trace.base
        );
        let computed = ann.score_trace.compute();
        assert!(
            (computed - 0.5).abs() < 1e-6,
            "trace.compute() must equal 0.5, got {computed}"
        );
        assert!(
            (ann.score - computed).abs() < 1e-6,
            "ann.score must equal trace.compute(): {} vs {computed}",
            ann.score
        );
    }

    struct FixedMultiplierCritic {
        multiplier: f32,
    }

    impl PlanCritic for FixedMultiplierCritic {
        fn name(&self) -> &'static str {
            "fixed_multiplier"
        }
        fn evaluate(
            &self,
            _plan: &TurnPlan,
            _ann: &PlanAnnotation,
            _ctx: &ScoringCtx,
        ) -> Option<CriticHit> {
            use crate::combat::ai::critics::overcommit_into_danger::OvercommitSource;
            Some(CriticHit {
                critic: CriticKind::OvercommitIntoDanger,
                multiplier: self.multiplier,
                reason: CriticReason::OvercommitIntoDanger {
                    source: OvercommitSource::SurvivalPath,
                    ratio: 0.5,
                },
            })
        }
    }

    /// Two critics [0.5, 0.8]; entry=1.0: ann.score ≈ 0.4, trace.compute() ≈ 0.4,
    /// both multiplier hits present in push order.
    #[test]
    fn p3a_critics_invariant_score_equals_compute_with_multiple_hits() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;

        let stage = CriticsStage {
            critics: vec![
                Box::new(FixedMultiplierCritic { multiplier: 0.5 }),
                Box::new(FixedMultiplierCritic { multiplier: 0.8 }),
            ],
        };
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![1.0]));

        let ann = &pool.annotations[0];
        // 1.0 * 0.5 * 0.8 = 0.4
        assert!(
            (ann.score - 0.4).abs() < 1e-5,
            "expected score 0.4 after two multipliers, got {}", ann.score
        );
        let computed = ann.score_trace.compute();
        assert!(
            (computed - 0.4).abs() < 1e-5,
            "trace.compute() must equal 0.4, got {computed}"
        );
        assert_eq!(ann.score_trace.multipliers.len(), 2, "expected 2 multiplier hits");
        assert_eq!(ann.score_trace.multipliers[0].kind, MultiplierKind::Critic);
        assert!(
            (ann.score_trace.multipliers[0].value - 0.5).abs() < 1e-6,
            "first multiplier value must be 0.5"
        );
        assert!(
            (ann.score_trace.multipliers[1].value - 0.8).abs() < 1e-6,
            "second multiplier value must be 0.8"
        );
    }

    /// Empty critics list: trace.base == entry_score, multipliers empty,
    /// trace.compute() == entry_score.
    #[test]
    fn p3a_critics_no_hits_leave_trace_with_only_base() {
        let entry_score = 0.75_f32;
        let stage = CriticsStage { critics: vec![] };
        let pool = apply_critics_to_pool(stage, make_pool_with_scores(vec![entry_score]));

        let ann = &pool.annotations[0];
        assert!(
            (ann.score_trace.base - entry_score).abs() < 1e-6,
            "trace.base must equal entry_score={entry_score}, got {}",
            ann.score_trace.base
        );
        assert!(
            ann.score_trace.multipliers.is_empty(),
            "multipliers must be empty when no critics fired"
        );
        assert!(
            (ann.score_trace.compute() - entry_score).abs() < 1e-6,
            "trace.compute() must equal entry_score={entry_score}"
        );
    }
}
