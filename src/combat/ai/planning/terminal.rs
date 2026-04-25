//! Terminal state evaluation — step 5 of ai-rework.
//!
//! One-shot per-plan evaluation of the final sim snapshot
//! (`plan.sim_snapshots.last()`). Independent of step-summed `PlanFactors`:
//! terminal axes capture "where we ended up", not "what we did along the way".
//!
//! Eight axes split into 3 clusters (5.1–5.3):
//!  - Defensive: `exposure_at_end`, `next_turn_lethality`
//!  - Offensive: `secure_kill`, `ally_rescue`, `board_control_gain`
//!  - Geometric: `line_actionability`, `density_value`, `pressure_spacing_zone`
//!
//! Step 5.0: scaffolding only — producer returns zeros, aggregator does not
//! read terminal scores yet. Wired into 5.4 via `axis_terminal_weights`.
//!
//! Decomposition: docs/ai_rework_step5_plan.md.

use serde::{Deserialize, Serialize};

use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

/// Terminal-state evaluation per plan. Producer is `terminal_state_score`;
/// each axis populated incrementally in 5.1–5.3. Consumed in
/// `finalize_scores` (5.4) via `axis_terminal_weights` × `NeedSignals`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TerminalScore {
    pub exposure_at_end: f32,
    pub next_turn_lethality: f32,
    pub secure_kill: f32,
    pub ally_rescue: f32,
    pub board_control_gain: f32,
    pub line_actionability: f32,
    pub density_value: f32,
    pub pressure_spacing_zone: f32,
}

/// Compute the terminal-state score for a plan from its final sim snapshot.
///
/// Step 5.0: returns `Default::default()` (zeros) — scaffolding, no consumer.
/// Step 5.1–5.3: each cluster fills in its axes.
pub fn terminal_state_score(
    _plan: &TurnPlan,
    _initial_snap: &BattleSnapshot,
    _ctx: &ScoringCtx,
) -> TerminalScore {
    TerminalScore::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_terminal_score_is_zero() {
        let t = TerminalScore::default();
        assert_eq!(t.exposure_at_end, 0.0);
        assert_eq!(t.next_turn_lethality, 0.0);
        assert_eq!(t.secure_kill, 0.0);
        assert_eq!(t.ally_rescue, 0.0);
        assert_eq!(t.board_control_gain, 0.0);
        assert_eq!(t.line_actionability, 0.0);
        assert_eq!(t.density_value, 0.0);
        assert_eq!(t.pressure_spacing_zone, 0.0);
    }
}
