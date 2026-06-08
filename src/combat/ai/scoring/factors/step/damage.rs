//! `StepFactor::Damage` — net damage value for a Cast step.
//!
//! Delegates to the shared offensive helper and reads the `damage` column.
//! Move steps return 0.0.

pub const NAME: &str = "damage";
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
    compute_offensive_for_step(ctx, step, outcome).damage
}

// Routing tests for all StepFactor variants live in factors::step::tests.
