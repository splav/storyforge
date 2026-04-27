//! `TerminalFactor::LineActionability` — fraction of enemies within max cast
//! range of actor's final position.

pub const NAME: &str = "line_actionability";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_line_actionability;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_line_actionability(plan, snap, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
