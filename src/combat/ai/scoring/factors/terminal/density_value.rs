//! `TerminalFactor::DensityValue` — enemy cluster density in AoE radius.

pub const NAME: &str = "density_value";
pub const SIGNED: bool = false;

use crate::combat::ai::scoring::factors::terminal_state::compute_density_value;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::orchestration::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_density_value(plan, snap, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
