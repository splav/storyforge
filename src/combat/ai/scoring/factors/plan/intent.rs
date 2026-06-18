//! `PlanFactor::Intent` — intent alignment score for the whole plan.
//!
//! Thin registry-API wrapper over `compute_plan_intent_sum`.

pub const NAME: &str = "intent";
pub const SIGNED: bool = true;

use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::aggregate::compute_plan_intent_sum;

pub fn compute(plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx) -> f32 {
    compute_plan_intent_sum(plan, intent, ctx, EvaluationMode::Default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::adapt::EvaluationMode;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::outcome::GeneratorAnnotation;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;

    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::content::content_view::ActiveContentData;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn build_idle_plan() -> TurnPlan {
        TurnPlan {
            steps: vec![],
            annotation: GeneratorAnnotation::default(),
            outcomes: vec![],
            sim_snapshots: vec![],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            partial_score: 0.0,
        }
    }

    #[test]
    fn plan_factor_compute_matches_legacy_intent() {
        let content = ActiveContentData::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = snapshot_from(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        let plan = build_idle_plan();
        let intent = TacticalIntent::Reposition;

        let via_leaf = compute(&plan, &intent, &ctx);
        let via_legacy = compute_plan_intent_sum(&plan, &intent, &ctx, EvaluationMode::Default);
        assert_eq!(
            via_leaf, via_legacy,
            "plan::intent leaf must match legacy compute_plan_intent_sum"
        );
    }
}
