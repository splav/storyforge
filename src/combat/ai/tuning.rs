//! AiTuning — central tuning data for AI scoring.
//! Populated incrementally across steps 2.2–2.6 (see docs/ai_rework_plan.md).

use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};

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

/// A lo→hi lerp curve: value = lo + (hi - lo) * clamp(t, 0, 1).
/// `lo` is the value at t=0 (low skill/instinct), `hi` at t=1 (max skill/instinct).
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct LerpCurve {
    pub lo: f32,
    pub hi: f32,
}

impl LerpCurve {
    pub fn eval(&self, t: f32) -> f32 {
        self.lo + (self.hi - self.lo) * t.clamp(0.0, 1.0)
    }
}

/// Lerp-curve parameters for DifficultyProfile derived values.
/// These replace the hardcoded endpoints inside difficulty.rs — the formulas
/// are unchanged, only the constants are now data-driven via AiTuning.
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Difficulty {
    /// Minimum pos_eval improvement to keep a Reposition candidate.
    /// Keyed on survival_instinct. lo=easy (low instinct), hi=epic (max instinct).
    pub reposition_min_improvement_curve: LerpCurve,
    /// HP% threshold for the hard-override panic gate.
    /// Low instinct → panics earlier (higher threshold).
    pub survival_hp_curve: LerpCurve,
    /// Danger threshold paired with the panic gate.
    /// Low awareness → needs more obvious danger to trigger (higher threshold).
    pub awareness_danger_curve: LerpCurve,
}

impl Default for LerpCurve {
    fn default() -> Self {
        // Defaults intentionally left as zeroes — each field in Difficulty
        // provides its own meaningful default via Difficulty::default().
        Self { lo: 0.0, hi: 0.0 }
    }
}

impl Default for Difficulty {
    fn default() -> Self {
        Self {
            reposition_min_improvement_curve: LerpCurve { lo: 0.30, hi: 0.12 },
            survival_hp_curve: LerpCurve { lo: 0.35, hi: 0.20 },
            awareness_danger_curve: LerpCurve { lo: 0.90, hi: 0.60 },
        }
    }
}

// ── Per-unit override scaffolding (step 2.7) ─────────────────────────────────

/// Per-unit partial override of AiTuning. Populated from `unit_templates.toml`
/// (field `ai_tuning_override`). All sub-sections are Option'd so individual
/// quirks can specify just the fields they tweak (Berserker: only `aoo_risk_floor`
/// raised; Coward: only `self_survival_epsilon` lowered, etc.).
///
/// Consumed via `AiTuning::apply_override` at `pick_action` time.
///
/// Scaffolding note (step 2.7): only `thresholds` is override-able in this
/// iteration. `difficulty` (LerpCurve) and `tables` (role-axis matrices)
/// intentionally omitted — add `Option<...Override>` fields here when a concrete
/// quirk needs them.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AiTuningOverride {
    #[serde(default)]
    pub thresholds: Option<ThresholdsOverride>,
    // hooks (not yet wired):
    // pub difficulty: Option<DifficultyOverride>,
    // pub tables: Option<TablesOverride>,
}

/// Per-unit partial override of `Thresholds`. Each field that is `Some(v)`
/// replaces the global value; `None` fields leave the global untouched.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ThresholdsOverride {
    #[serde(default)] pub survival_floor: Option<f32>,
    #[serde(default)] pub low_hp_factor: Option<f32>,
    #[serde(default)] pub aoo_penalty_k: Option<f32>,
    #[serde(default)] pub aoo_risk_floor: Option<f32>,
    #[serde(default)] pub self_survival_epsilon: Option<f32>,
    #[serde(default)] pub mild_penalty: Option<f32>,
    #[serde(default)] pub stickiness_bonus: Option<f32>,
    #[serde(default)] pub target_stickiness_bonus: Option<f32>,
    #[serde(default)] pub max_committed_turns: Option<u8>,
}

impl AiTuning {
    /// Produce a per-unit AiTuning by overlaying `ov` on `self`. `None` fields
    /// inherit from `self`, `Some(v)` fields replace.
    /// Explicit per-field merge — no derive-magic.
    pub fn apply_override(&self, ov: &AiTuningOverride) -> AiTuning {
        let mut out = self.clone();
        if let Some(t) = &ov.thresholds {
            if let Some(v) = t.survival_floor         { out.thresholds.survival_floor = v; }
            if let Some(v) = t.low_hp_factor          { out.thresholds.low_hp_factor = v; }
            if let Some(v) = t.aoo_penalty_k          { out.thresholds.aoo_penalty_k = v; }
            if let Some(v) = t.aoo_risk_floor         { out.thresholds.aoo_risk_floor = v; }
            if let Some(v) = t.self_survival_epsilon  { out.thresholds.self_survival_epsilon = v; }
            if let Some(v) = t.mild_penalty           { out.thresholds.mild_penalty = v; }
            if let Some(v) = t.stickiness_bonus       { out.thresholds.stickiness_bonus = v; }
            if let Some(v) = t.target_stickiness_bonus { out.thresholds.target_stickiness_bonus = v; }
            if let Some(v) = t.max_committed_turns    { out.thresholds.max_committed_turns = v; }
        }
        // hooks: difficulty and tables override would be applied here.
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_override_empty_is_identity() {
        let base = AiTuning::default();
        let result = base.apply_override(&AiTuningOverride::default());
        // Check thresholds
        assert_eq!(result.thresholds.survival_floor, base.thresholds.survival_floor);
        assert_eq!(result.thresholds.max_committed_turns, base.thresholds.max_committed_turns);
        // Check difficulty
        assert_eq!(result.difficulty.survival_hp_curve.lo, base.difficulty.survival_hp_curve.lo);
        // Check tables (first element of axis_factor_weights and axis_position_weights)
        assert_eq!(result.tables.axis_factor_weights[0][0], base.tables.axis_factor_weights[0][0]);
        assert_eq!(result.tables.axis_position_weights[0][0], base.tables.axis_position_weights[0][0]);
    }

    #[test]
    fn apply_override_partial_thresholds() {
        let base = AiTuning::default();
        let ov = AiTuningOverride {
            thresholds: Some(ThresholdsOverride {
                survival_floor: Some(0.5),
                aoo_risk_floor: Some(0.9),
                ..Default::default()
            }),
        };
        let result = base.apply_override(&ov);

        // Overridden fields
        assert_eq!(result.thresholds.survival_floor, 0.5);
        assert_eq!(result.thresholds.aoo_risk_floor, 0.9);

        // Untouched thresholds — must equal default
        let def = Thresholds::default();
        assert_eq!(result.thresholds.low_hp_factor,           def.low_hp_factor);
        assert_eq!(result.thresholds.aoo_penalty_k,           def.aoo_penalty_k);
        assert_eq!(result.thresholds.self_survival_epsilon,   def.self_survival_epsilon);
        assert_eq!(result.thresholds.mild_penalty,            def.mild_penalty);
        assert_eq!(result.thresholds.stickiness_bonus,        def.stickiness_bonus);
        assert_eq!(result.thresholds.target_stickiness_bonus, def.target_stickiness_bonus);
        assert_eq!(result.thresholds.max_committed_turns,     def.max_committed_turns);

        // Difficulty and tables untouched
        assert_eq!(result.difficulty.survival_hp_curve.lo, base.difficulty.survival_hp_curve.lo);
        assert_eq!(result.tables.axis_factor_weights[0][0], base.tables.axis_factor_weights[0][0]);
    }

    #[test]
    fn apply_override_toml_roundtrip() {
        let toml_src = "[thresholds]\nsurvival_floor = 0.5\n";
        let ov: AiTuningOverride = toml::from_str(toml_src)
            .expect("AiTuningOverride should parse from TOML");
        let result = AiTuning::default().apply_override(&ov);
        assert_eq!(result.thresholds.survival_floor, 0.5);
        // Other thresholds unchanged
        assert_eq!(result.thresholds.aoo_risk_floor, Thresholds::default().aoo_risk_floor);
    }
}
