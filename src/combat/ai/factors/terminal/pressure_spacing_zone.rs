//! `TerminalFactor::PressureSpacingZone` — signed change in ally-support map
//! value (start → final position).
//!
//! `snap` is unused; bound as `_snap` for macro-uniform signature.

pub const NAME: &str = "pressure_spacing_zone";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_pressure_spacing_zone;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, _snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_pressure_spacing_zone(plan, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
