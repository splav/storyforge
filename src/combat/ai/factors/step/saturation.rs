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

// Routing tests for all StepFactor variants live in factors::step::tests.
