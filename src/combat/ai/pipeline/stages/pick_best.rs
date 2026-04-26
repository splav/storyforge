//! PickBestStage — step 7.4.
//!
//! Selects the winning plan from the scored pool using the same mercy + top-K
//! window logic that `PlanRanking::pick` used (via `pick_best_plan`). Writes
//! `annotation.chosen = true` and `annotation.pick = Some(PickInfo { .. })`
//! on the winning plan.

use crate::combat::ai::outcome::PickInfo;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::pick_best_plan;

pub struct PickBestStage;

impl PlanStage for PickBestStage {
    fn name(&self) -> &'static str {
        "pick_best"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        if pool.is_empty() {
            return;
        }

        let scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.raw_factors).collect();

        let (best_idx, mech) = pick_best_plan(&scores, &raw_factors, ctx.scoring.world, ctx.rng);

        pool.annotations[best_idx].chosen = true;
        pool.annotations[best_idx].pick = Some(PickInfo { mechanics: mech });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::PlanFactors;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn run_pick(scores: Vec<f32>) -> ScoredPool {
        let n = scores.len();
        let plans = vec![TurnPlan::default(); n];
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
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip(scores.into_iter()) {
            ann.score = score;
            ann.raw_factors = PlanFactors::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);
        pool
    }

    #[test]
    fn pick_best_marks_exactly_one_chosen() {
        let pool = run_pick(vec![0.3, 0.8, 0.5]);
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");
    }

    #[test]
    fn pick_best_selects_highest_score() {
        // With deterministic DiceRng seed and no mercy margin (default difficulty),
        // the highest-scored plan should be chosen.
        let pool = run_pick(vec![0.1, 0.9, 0.4]);
        // Index 1 has the highest score.
        assert!(pool.annotations[1].chosen, "highest-scored plan should be chosen");
        assert!(pool.annotations[1].pick.is_some(), "chosen plan should have PickInfo");
    }

    #[test]
    fn pick_best_noop_on_empty_pool() {
        let pool = run_pick(vec![]);
        assert_eq!(pool.len(), 0);
    }
}
