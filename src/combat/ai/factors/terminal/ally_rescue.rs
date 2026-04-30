//! `TerminalFactor::AllyRescue` — credit for rescuing an endangered ally.

pub const NAME: &str = "ally_rescue";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_ally_rescue;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_ally_rescue(plan, snap, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
