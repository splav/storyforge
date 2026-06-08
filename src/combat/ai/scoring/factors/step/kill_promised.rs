//! `StepFactor::KillPromised` — probability of kill on next round (DoT/low-HP).

pub const NAME: &str = "kill_promised";
pub const SIGNED: bool = false;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::scoring::factors::{compute_offensive_for_step, ScoredStep};

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    compute_offensive_for_step(ctx, step, outcome).kill_promised
}

// Routing tests for all StepFactor variants live in factors::step::tests.
