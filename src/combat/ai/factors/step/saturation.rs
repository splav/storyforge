//! `StepFactor::Saturation` — buff-redundancy penalty for a Cast step.
//!
//! Wraps `buff_saturation_penalty`. Move steps and non-Cast steps return 0.0.
//!
//! # Contract on `ctx`
//! `ctx.snap` must be the **pre-step** snapshot. The caller (scorer's step loop)
//! shifts perspective via `ctx.with_perspective(&sim_actor, pre_snap)` before
//! calling this factor, so `ctx.snap` reflects the state before this step fires.

pub const NAME: &str = "saturation";
pub const SIGNED: bool = true;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::factors::buff_saturation_penalty;
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    _outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    match step {
        ScoredStep::Cast { ability, target, .. } => {
            let caster = ctx.active.entity;
            let pre_snap = ctx.snap; // caller must have applied with_perspective
            buff_saturation_penalty(ability, *target, caster, pre_snap, ctx.world.content)
        }
        ScoredStep::Move { .. } => 0.0,
    }
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
        assert_eq!(compute(&ctx, &step, &outcome, &needs), 0.0, "move step yields zero saturation");
    }
}
