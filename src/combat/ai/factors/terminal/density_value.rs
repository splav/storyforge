//! `TerminalFactor::DensityValue` — enemy cluster density in AoE radius.

pub const NAME: &str = "density_value";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_density_value;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_density_value(plan, snap, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
