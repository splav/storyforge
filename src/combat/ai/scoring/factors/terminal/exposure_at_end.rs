//! `TerminalFactor::ExposureAtEnd` — danger-map value at final position.
//!
//! `snap` is unused; bound as `_snap` for macro-uniform signature.

pub const NAME: &str = "exposure_at_end";
pub const SIGNED: bool = false;

use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::terminal_state::compute_exposure_at_end;
use crate::combat::ai::world::snapshot::BattleSnapshot;

pub fn compute(plan: &TurnPlan, _snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_exposure_at_end(plan, ctx)
}

// Behavioural tests for `compute_exposure_at_end` live in
// `planning::terminal::tests` — this leaf is a pure routing wrapper.
