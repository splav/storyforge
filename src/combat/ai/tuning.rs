//! AiTuning — central tuning data for AI scoring.
//! Populated incrementally across steps 2.2–2.6 (see docs/ai_rework_plan.md).

use bevy::prelude::Resource;
use serde::Deserialize;

#[derive(Resource, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct AiTuning {
    pub thresholds: Thresholds,
    pub tables: Tables,
    pub difficulty: Difficulty,
}

/// Scalar thresholds used by the AI scoring/sanity pipeline.
/// Populated in steps 2.2 (sanity.rs) and 2.3 (intent.rs).
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Thresholds {
    /// Minimum multiplier applied by survival quadratic.
    pub survival_floor: f32,
    /// Amplifies the HP × danger² product.
    pub low_hp_factor: f32,
    /// AoO-penalty shape constant.
    pub aoo_penalty_k: f32,
    /// Floor for the AoO-risk (non-lethal) multiplier.
    pub aoo_risk_floor: f32,
    /// Minimum `self_survival` for a plan to be considered defensive under ProtectSelf.
    pub self_survival_epsilon: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            survival_floor: 0.25,
            low_hp_factor: 1.2,
            aoo_penalty_k: 2.0,
            aoo_risk_floor: 0.25,
            self_survival_epsilon: 0.15,
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Tables {
    // populated in step 2.4 (role factor weights) and 2.5 (position eval weights).
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Difficulty {
    // populated in step 2.6 (DifficultyProfile lerp curves).
}
