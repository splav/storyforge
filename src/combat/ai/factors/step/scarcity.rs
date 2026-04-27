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

// Routing tests for all StepFactor variants live in factors::step::tests.
