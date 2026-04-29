//! CriticsStage — step 10.0.
//!
//! Dispatches each plan through a registered `Vec<Box<dyn PlanCritic>>`.
//! For each critic hit, applies `ann.score *= hit.multiplier` and pushes the
//! hit into `ann.critics` for structured logging.
//!
//! `CriticsStage::first_wave()` starts with an empty vec (step 10.0 scaffolding).
//! Concrete critics are registered in steps 10.1–10.3.

use crate::combat::ai::critics::blindspot_ranged::BlindspotRanged;
use crate::combat::ai::critics::overcommit_into_danger::OvercommitIntoDanger;
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
    /// Step 10.3 will add resource/value critics.
    pub fn first_wave() -> Self {
        Self {
            critics: vec![
                Box::new(OvercommitIntoDanger),
                Box::new(SelfLethalWithoutPayoff),
                Box::new(BlindspotRanged),
            ],
        }
    }
}

impl PlanStage for CriticsStage {
    fn name(&self) -> &'static str {
        "critics"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            for c in &self.critics {
                if let Some(hit) = c.evaluate(plan, ann, ctx.scoring) {
                    ann.score *= hit.multiplier;
                    ann.critics.push(hit);
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
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
}
