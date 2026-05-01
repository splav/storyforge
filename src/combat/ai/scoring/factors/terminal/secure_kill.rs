//! `TerminalFactor::SecureKill` — sum of kill confidence across all steps.
//!
//! Both `snap` and `ctx` are unused; bound as `_snap`/`_ctx` for macro-uniform
//! signature.

pub const NAME: &str = "secure_kill";
pub const SIGNED: bool = false;

use crate::combat::ai::scoring::factors::terminal_state::compute_secure_kill;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::orchestration::ScoringCtx;

pub fn compute(plan: &TurnPlan, _snap: &BattleSnapshot, _ctx: &ScoringCtx) -> f32 {
    compute_secure_kill(plan)
}

// Behavioural tests live in planning::terminal::tests — this leaf is a pure routing wrapper.
