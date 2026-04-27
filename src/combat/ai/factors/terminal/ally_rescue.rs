//! `TerminalFactor::AllyRescue` — credit for rescuing an endangered ally.

pub const NAME: &str = "ally_rescue";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_ally_rescue;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_ally_rescue(plan, snap, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn idle_plan(pos: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            annotation: PlanAnnotation::default(),
            outcomes: vec![],
            sim_snapshots: vec![],
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            partial_score: 0.0,
        }
    }

    #[test]
    fn terminal_factor_compute_matches_legacy_ally_rescue() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        let plan = idle_plan(tile);
        let via_leaf = compute(&plan, &snap, &ctx);
        let via_legacy = compute_ally_rescue(&plan, &snap, &ctx);
        assert_eq!(via_leaf, via_legacy, "ally_rescue leaf must match legacy");
    }
}
