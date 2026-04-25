//! Appraisal / Need layer (step 3 of ai-rework).
//!
//! Aggregates raw tactical facts (`BattleSnapshot` + `InfluenceMaps` + `AiMemory`)
//! into normalised "urgency" signals consumed by `select_intent` and downstream
//! scoring layers. Producer is `compute_need_signals`; consumers are wired in
//! steps 3.2–3.5. Until then the producer returns `Default::default()` (zeros).
//!
//! Spec: `docs/ai_need_signals.md` (mining-driven taxonomy + curve params).
//! Decomposition: `docs/ai_rework_step3_plan.md`.

use serde::{Deserialize, Serialize};

use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::tuning::AiTuning;

/// Normalised need-signal vector. Each field in [0, 1] semantically; producer
/// clamps. Five signals are populated in step 3.1 (`self_preserve`,
/// `finish_target`, `reposition`, `conserve_resource`, `continue_commitment`);
/// the remaining three (`rescue_ally`, `apply_cc`, `setup_aoe`) stay at 0.0
/// until the second mining iteration delivers concrete inputs
/// (see `ai_need_signals.md:166`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NeedSignals {
    pub self_preserve: f32,
    pub rescue_ally: f32,
    pub finish_target: f32,
    pub apply_cc: f32,
    pub setup_aoe: f32,
    pub reposition: f32,
    pub conserve_resource: f32,
    pub continue_commitment: f32,
}

/// Compute need signals from raw tactical state.
///
/// Step 3.0: returns zeros — scaffolding only, no consumer reads the result.
/// Step 3.1: implements all 5 mineable signals (see plan §3.1).
pub fn compute_need_signals(
    _active: &UnitSnapshot,
    _snap: &BattleSnapshot,
    _maps: &InfluenceMaps,
    _memory: &AiMemory,
    _tuning: &AiTuning,
) -> NeedSignals {
    NeedSignals::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_need_signals_are_zero() {
        let n = NeedSignals::default();
        assert_eq!(n.self_preserve, 0.0);
        assert_eq!(n.rescue_ally, 0.0);
        assert_eq!(n.finish_target, 0.0);
        assert_eq!(n.apply_cc, 0.0);
        assert_eq!(n.setup_aoe, 0.0);
        assert_eq!(n.reposition, 0.0);
        assert_eq!(n.conserve_resource, 0.0);
        assert_eq!(n.continue_commitment, 0.0);
    }
}
