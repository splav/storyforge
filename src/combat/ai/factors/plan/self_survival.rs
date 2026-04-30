//! `PlanFactor::SelfSurvival` — how much the plan improves the actor's survival.
//!
//! Thin wrapper over `factors::compute_plan_self_survival`. The `intent`
//! parameter is **not used** — it is present only for trait-uniformity with the
//! other `PlanFactor` compute signatures.
//!
//! `_intent` is unused — required for `factor_kind!` macro uniformity.
//! Pinned by `self_survival_ignores_intent_parameter` test.

pub const NAME: &str = "self_survival";
pub const SIGNED: bool = false;

use crate::combat::ai::factors::compute_plan_self_survival;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, _intent: &TacticalIntent, ctx: &ScoringCtx) -> f32 {
    compute_plan_self_survival(plan, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    fn idle_plan() -> TurnPlan {
        TurnPlan {
            steps: vec![],
            annotation: PlanAnnotation::default(),
            outcomes: vec![],
            sim_snapshots: vec![],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            partial_score: 0.0,
        }
    }

    #[test]
    fn plan_factor_compute_matches_legacy_self_survival() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        let plan = idle_plan();
        let intent = TacticalIntent::Reposition;

        let via_leaf = compute(&plan, &intent, &ctx);
        let via_legacy = compute_plan_self_survival(&plan, &ctx);
        assert_eq!(
            via_leaf, via_legacy,
            "plan::self_survival leaf must match legacy compute_plan_self_survival"
        );
    }

    #[test]
    fn self_survival_ignores_intent_parameter() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        let plan = idle_plan();

        // Two different intents should produce identical self_survival.
        let dummy_target = Entity::from_raw_u32(99).unwrap();
        let intent_a = TacticalIntent::Reposition;
        let intent_b = TacticalIntent::FocusTarget { target: dummy_target };

        let score_a = compute(&plan, &intent_a, &ctx);
        let score_b = compute(&plan, &intent_b, &ctx);
        assert_eq!(
            score_a, score_b,
            "self_survival must not depend on intent; got {score_a} vs {score_b}"
        );
    }
}
