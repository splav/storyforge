//! `StepFactor::Damage` — net damage value for a Cast step.
//!
//! Delegates to the shared offensive helper and reads the `damage` column.
//! Move steps return 0.0.

pub const NAME: &str = "damage";
pub const SIGNED: bool = false;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::factors::{compute_offensive_for_step, ScoredStep};
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    compute_offensive_for_step(ctx, step, outcome).damage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::outcome::ActionOutcomeEstimate;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn step_factor_compute_pure_for_known_outcome() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        // Move step → always 0
        let step = ScoredStep::Move { caster_tile: tile };
        let outcome = ActionOutcomeEstimate::default();
        let needs = NeedSignals::default();
        assert_eq!(compute(&ctx, &step, &outcome, &needs), 0.0, "move step yields zero damage");
    }
}
