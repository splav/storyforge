//! `TerminalFactor::NextTurnLethality` — fraction of actor HP that can be dealt
//! by reachable enemies next turn.

pub const NAME: &str = "next_turn_lethality";
pub const SIGNED: bool = false;

use crate::combat::ai::planning::terminal::compute_next_turn_lethality;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
    compute_next_turn_lethality(plan, snap, ctx)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
