//! `TerminalFactor::ExposureAtEnd` — danger-map value at final position.
//!
//! `snap` is unused; bound as `_snap` for macro-uniform signature.

pub const NAME: &str = "exposure_at_end";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_exposure_at_end;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, _snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_exposure_at_end(plan, ctx)
}

// Behavioural tests for `compute_exposure_at_end` live in
// `planning::terminal::tests` — this leaf is a pure routing wrapper.
