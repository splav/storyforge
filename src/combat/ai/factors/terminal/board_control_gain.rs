//! `TerminalFactor::BoardControlGain` — signed change in opportunity-map value.
//!
//! `snap` is unused; bound as `_snap` for macro-uniform signature.

pub const NAME: &str = "board_control_gain";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_board_control_gain;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, _snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_board_control_gain(plan, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
