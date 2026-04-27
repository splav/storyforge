//! `PlanFactor::TempoGain` — plan-terminal approach + exit-danger bonus.
//!
//! Thin wrapper over `factors::compute_plan_tempo_gain`. The `intent` parameter
//! is used (forwarded to the tempo calculation, which needs the intent target).

pub const NAME: &str = "tempo_gain";
pub const SIGNED: bool = true;

use crate::combat::ai::factors::compute_plan_tempo_gain;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx) -> f32 {
    compute_plan_tempo_gain(plan, intent, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

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
    fn plan_factor_compute_matches_legacy_tempo_gain() {
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
        let via_legacy = compute_plan_tempo_gain(&plan, &intent, &ctx);
        assert_eq!(
            via_leaf, via_legacy,
            "plan::tempo_gain leaf must match legacy compute_plan_tempo_gain"
        );
    }
}
