//! `StepFactor::Scarcity` — resource-vs-swing justification for Cast.
//!
//! Reads `outcome.p_kill_now` as the kill signal (same source as `KillNow`
//! factor) and delegates to `compute_scarcity`. Move steps return 0.0.
//!
//! # Contract on `ctx`
//! `ctx` must use the pre-step snapshot (shifted via `with_perspective` by the
//! caller). This matches legacy `compute_plan_factors_sans_intent` where the
//! scarcity call sat inside the `with_perspective` block.

pub const NAME: &str = "scarcity";
pub const SIGNED: bool = true;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::factors::scarcity::compute_scarcity;
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    // Derive kill_now from the same outcome fact that KillNow uses.
    let kill_now = outcome.p_kill_now;
    compute_scarcity(step, kill_now, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::ActionOutcomeEstimate;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::combat::ai::difficulty::DifficultyProfile;
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

        // Move → always 0
        let step = ScoredStep::Move { caster_tile: tile };
        let outcome = ActionOutcomeEstimate::default();
        let needs = NeedSignals::default();
        assert_eq!(compute(&ctx, &step, &outcome, &needs), 0.0, "move step yields zero scarcity");
    }
}
