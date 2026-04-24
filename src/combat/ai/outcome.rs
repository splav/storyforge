//! ActionOutcomeEstimate — structured outcome vector shared across factors,
//! intent, critics, and terminal eval. Populated in SimState::apply_step
//! call chain; consumers migrate onto it incrementally (steps 4.1–4.5).
//!
//! Step 4.0 ships the type + PlanAnnotation container zero-filled — no
//! consumers yet. See docs/ai_rework_step4_plan.md.

use serde::{Deserialize, Serialize};

/// Structured estimate of a single plan step's consequences.
/// Fields populated incrementally across steps 4.1–4.2.
///
/// Semantic note (docs/ai_rework.md §4):
/// - `expected_damage`: raw expected damage from this step (step 4.1).
/// - `p_kill_now`: 1.0 if step kills a target in this turn, else 0.0.
/// - `p_kill_soon`: probability of killing a target within the damage horizon.
/// - `deny_value`: aggregated CC / armor-debuff / vuln "denial" value.
/// - `rescue_value`: heal value with urgency baked-in during wave 1;
///   step 3 (need layer) will split urgency into NeedSignals.rescue_ally.
/// - `board_pressure`: 0.0 placeholder, filled in step 5 (terminal eval).
/// - `exposure_delta`: Δdanger from step (worst_path_danger for Move, 0 for Cast).
/// - `geometry_gain`: 0.0 placeholder, filled in step 17 (geometry awareness).
/// - `resource_swing`: signed resource cost (negative = spent).
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    pub expected_damage: f32,
    pub p_kill_now: f32,
    pub p_kill_soon: f32,
    pub deny_value: f32,
    pub rescue_value: f32,
    pub board_pressure: f32,
    pub exposure_delta: f32,
    pub geometry_gain: f32,
    pub resource_swing: f32,
}

/// Per-plan annotation bundle. Grows as pipeline stages accrue data
/// (outcome in wave 1; critics / band / agenda in later waves).
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlanAnnotation {
    /// One ActionOutcomeEstimate per plan step, same length as TurnPlan.steps
    /// and TurnPlan.outcomes.
    #[serde(default)]
    pub outcomes: Vec<ActionOutcomeEstimate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_outcome_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.expected_damage, 0.0);
        assert_eq!(o.p_kill_now, 0.0);
        assert_eq!(o.p_kill_soon, 0.0);
        assert_eq!(o.deny_value, 0.0);
        assert_eq!(o.rescue_value, 0.0);
        assert_eq!(o.board_pressure, 0.0);
        assert_eq!(o.exposure_delta, 0.0);
        assert_eq!(o.geometry_gain, 0.0);
        assert_eq!(o.resource_swing, 0.0);
    }

    #[test]
    fn default_annotation_is_empty() {
        let a = PlanAnnotation::default();
        assert!(a.outcomes.is_empty());
    }
}
