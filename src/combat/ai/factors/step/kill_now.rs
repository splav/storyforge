//! `StepFactor::KillNow` — probability of killing target this step.

pub const NAME: &str = "kill_now";
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
    compute_offensive_for_step(ctx, step, outcome).kill_now
}

// Routing tests for all StepFactor variants live in factors::step::tests.
