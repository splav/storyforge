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
    /// Penalty for wrong-ally heal in ProtectAlly / non-AoE under SetupAOE.
    pub mild_penalty: f32,
    /// Bonus multiplier for continuing the same intent (stickiness).
    pub stickiness_bonus: f32,
    /// Same target bonus on top of stickiness.
    pub target_stickiness_bonus: f32,
    /// Max turns an intent can receive stickiness bonus.
    pub max_committed_turns: u8,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            survival_floor: 0.25,
            low_hp_factor: 1.2,
            aoo_penalty_k: 2.0,
            aoo_risk_floor: 0.25,
            self_survival_epsilon: 0.15,
            mild_penalty: -0.3,
            stickiness_bonus: 0.25,
            target_stickiness_bonus: 0.15,
            max_committed_turns: 3,
        }
    }
}

/// Tabular weights keyed by role-axis. Rows = axes (Tank, Melee, Ranged,
/// Control, Support). Columns depend on the table.
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Tables {
    /// Per-axis weights for the 10 utility factors.
    /// Columns: [damage, kill_now, kill_promised, cc, heal, intent,
    /// scarcity, tempo_gain, saturation, self_survival].
    pub axis_factor_weights: [[f32; 10]; 5],
    /// Per-axis weights for the 3 influence maps used in position evaluation.
    /// Columns: [danger, ally_support, opportunity].
    pub axis_position_weights: [[f32; 3]; 5],
}

impl Default for Tables {
    #[rustfmt::skip]
    fn default() -> Self {
        Self {
            axis_factor_weights: [
                //  dmg   kn    kp    cc    heal  intent scarc tempo sat   surv
                [   0.4,  0.6,  0.3,  0.5,  0.2,  1.0,  0.4,  0.8,  1.0,  1.0 ], // Tank
                [   1.3,  1.6,  0.8,  0.2,  0.0,  1.0,  0.3,  1.0,  1.0,  0.8 ], // Melee
                [   1.3,  1.3,  0.65, 0.3,  0.0,  1.0,  0.5,  1.2,  1.0,  0.8 ], // Ranged
                [   0.4,  0.5,  0.4,  1.6,  0.0,  1.0,  1.2,  1.0,  1.0,  0.8 ], // Control
                [   0.2,  0.3,  0.15, 0.6,  2.0,  1.0,  0.8,  0.8,  1.0,  1.2 ], // Support
            ],
            axis_position_weights: [
                //  danger  ally   opp
                [   -1.0,   0.7,   0.9 ], // Tank
                [   -0.9,   0.4,   1.5 ], // Melee
                [   -1.8,   0.7,   1.0 ], // Ranged
                [   -1.5,   0.8,   0.8 ], // Control
                [   -2.5,   1.3,   0.5 ], // Support
            ],
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Difficulty {
    // populated in step 2.6 (DifficultyProfile lerp curves).
}
