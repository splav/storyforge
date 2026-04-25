//! AiTuning — central tuning data for AI scoring.
//! Populated incrementally across steps 2.2–2.6 (see docs/ai_rework_plan.md).

use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};

// ── Response curves (step 3.0 appraisal layer) ───────────────────────────────

/// A parameterised transfer function mapping a raw input scalar to a [0, 1]
/// normalised "urgency" value. Used by `compute_need_signals` to convert
/// tactical facts into need signals.
///
/// Two forms cover all current mining requirements (see `ai_need_signals.md:155`).
/// Additional forms (e.g. power, exponential decay) can be added in future mining
/// iterations per `ai_rework_plan.md:373`.
#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseCurve {
    /// Sigmoid: `eval(x) = 1 / (1 + exp(-k * (x - mid)))`.
    /// `k > 0`: ascending (low at x < mid, high at x > mid).
    /// `k < 0`: descending (high at x < mid, low at x > mid).
    Logistic { mid: f32, k: f32 },
    /// Piecewise linear: 0 below `x_lo`, 1 above `x_hi`, linear interp between.
    /// `x_lo == x_hi`: step function at that point.
    LinearClamped { x_lo: f32, x_hi: f32 },
}

impl ResponseCurve {
    pub fn eval(&self, x: f32) -> f32 {
        match self {
            ResponseCurve::Logistic { mid, k } => {
                1.0 / (1.0 + (-k * (x - mid)).exp())
            }
            ResponseCurve::LinearClamped { x_lo, x_hi } => {
                if (x_hi - x_lo).abs() < f32::EPSILON {
                    if x >= *x_lo { 1.0 } else { 0.0 }
                } else {
                    ((x - x_lo) / (x_hi - x_lo)).clamp(0.0, 1.0)
                }
            }
        }
    }
}

/// Response-curve parameters for the appraisal / need layer (step 3).
/// Each field describes how a raw tactical input maps to a [0, 1] urgency signal.
/// Stub parameters — will be tuned by mining metrics in step 3.6.
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Curves {
    /// Logistic over `(1.0 - hp_pct)`. High at low HP. Used in 3.1 producer.
    pub self_preserve_hp: ResponseCurve,
    /// Scalar α: `self_preserve` gets multiplied by `(1 + α * recent_damage_taken)`.
    pub self_preserve_dmg_alpha: f32,
    /// Logistic over `last_target.hp_pct()` with `k > 0`. High while the target
    /// is alive and healthy (≥ ~0.5 hp), drops as it nears the finisher zone.
    /// The hp ≤ 0.25 finisher cutoff is enforced by an explicit gate in
    /// `compute_need_signals` before this curve is evaluated.
    pub continue_commitment_hp: ResponseCurve,
    /// Logistic over `(1.0 - target.hp_pct())`. High when killable target is low HP.
    pub finish_target_kill: ResponseCurve,
    /// LinearClamped over `best_position_improvement` (delta of `evaluate_position`).
    pub reposition_pos_gain: ResponseCurve,
    /// Logistic over `mana_ratio` with `k < 0`. High at low resources.
    pub conserve_resource: ResponseCurve,
}

impl Default for Curves {
    fn default() -> Self {
        Self {
            self_preserve_hp: ResponseCurve::Logistic { mid: 0.5, k: 8.0 },
            self_preserve_dmg_alpha: 0.6,
            continue_commitment_hp: ResponseCurve::Logistic { mid: 0.4, k: 10.0 },
            finish_target_kill: ResponseCurve::Logistic { mid: 0.6, k: 6.0 },
            reposition_pos_gain: ResponseCurve::LinearClamped { x_lo: 0.05, x_hi: 0.5 },
            conserve_resource: ResponseCurve::Logistic { mid: 0.3, k: -10.0 },
        }
    }
}

#[derive(Resource, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct AiTuning {
    pub thresholds: Thresholds,
    pub tables: Tables,
    pub difficulty: Difficulty,
    /// Response curves for the appraisal / need layer (step 3.0).
    pub curves: Curves,
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
    /// Below this `hp_pct` an actor is considered "in low-HP zone" for memory
    /// tracking (see `AiMemory.turns_in_low_hp`). Used by the 3.1 producer.
    pub low_hp_zone_threshold: f32,
    /// Threshold for the panic override gate. When `need_signals.self_preserve`
    /// reaches this AND danger is above `awareness_danger_threshold`, the AI
    /// bypasses scoring and forces `ProtectSelf`. Migrated from the old
    /// `hp_pct < survival_hp_threshold` gate; calibrated so the old condition
    /// (hp ≈ 0.20, danger ≈ 0.6) maps to the same trigger point on the new
    /// logistic curve. Step 3.2 consumer.
    pub panic_self_preserve_threshold: f32,
    /// Soft floor for the ProtectSelf intent. Below this `self_preserve`
    /// magnitude the soft branch doesn't even consider ProtectSelf.
    /// Migrated from the old `hp_pct < 0.4` gate. Step 3.2 consumer.
    pub soft_self_preserve_threshold: f32,
    /// Soft floor for the Reposition intent. Below this `need_signals.reposition`
    /// magnitude the branch doesn't even consider Reposition. Migrated from the
    /// old `pos_eval < awareness_reposition_threshold()` gate. Step 3.4 consumer.
    pub reposition_signal_floor: f32,
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
            low_hp_zone_threshold: 0.4,
            panic_self_preserve_threshold: 0.85,
            soft_self_preserve_threshold: 0.2,
            reposition_signal_floor: 0.1,
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
    #[serde(default)] pub panic_self_preserve_threshold: Option<f32>,
    #[serde(default)] pub soft_self_preserve_threshold: Option<f32>,
    #[serde(default)] pub reposition_signal_floor: Option<f32>,
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
            if let Some(v) = t.panic_self_preserve_threshold { out.thresholds.panic_self_preserve_threshold = v; }
            if let Some(v) = t.soft_self_preserve_threshold  { out.thresholds.soft_self_preserve_threshold = v; }
            if let Some(v) = t.reposition_signal_floor       { out.thresholds.reposition_signal_floor = v; }
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

    // ── ResponseCurve tests ───────────────────────────────────────────────────

    #[test]
    fn response_curve_logistic_at_mid_returns_half() {
        let c = ResponseCurve::Logistic { mid: 0.5, k: 8.0 };
        let v = c.eval(0.5);
        // At x == mid, logistic = 0.5 exactly.
        assert!((v - 0.5).abs() < 1e-6, "expected 0.5, got {v}");
    }

    #[test]
    fn response_curve_logistic_ascending_low_at_zero_high_at_one() {
        // k > 0: low at x=0, high at x=1 (relative to mid=0.5).
        let c = ResponseCurve::Logistic { mid: 0.5, k: 8.0 };
        let low = c.eval(0.0);
        let high = c.eval(1.0);
        assert!(low < 0.1, "expected low < 0.1, got {low}");
        assert!(high > 0.9, "expected high > 0.9, got {high}");
    }

    #[test]
    fn response_curve_logistic_descending_inverted() {
        // k < 0: high at x=0 (below mid), low at x=1 (above mid).
        let c = ResponseCurve::Logistic { mid: 0.5, k: -8.0 };
        let high = c.eval(0.0);
        let low = c.eval(1.0);
        assert!(high > 0.9, "expected high > 0.9, got {high}");
        assert!(low < 0.1, "expected low < 0.1, got {low}");
    }

    #[test]
    fn response_curve_linear_clamped_zero_below_lo_one_above_hi() {
        let c = ResponseCurve::LinearClamped { x_lo: 0.1, x_hi: 0.8 };
        assert_eq!(c.eval(0.0), 0.0);
        assert_eq!(c.eval(0.05), 0.0);
        assert_eq!(c.eval(1.0), 1.0);
        assert_eq!(c.eval(0.9), 1.0);
    }

    #[test]
    fn response_curve_linear_clamped_lerp_at_midpoint() {
        let c = ResponseCurve::LinearClamped { x_lo: 0.0, x_hi: 1.0 };
        let v = c.eval(0.5);
        assert!((v - 0.5).abs() < 1e-6, "expected 0.5, got {v}");
    }

    #[test]
    fn response_curve_linear_clamped_step_when_lo_eq_hi() {
        let c = ResponseCurve::LinearClamped { x_lo: 0.5, x_hi: 0.5 };
        // Below the step point → 0.
        assert_eq!(c.eval(0.4), 0.0);
        // At or above → 1.
        assert_eq!(c.eval(0.5), 1.0);
        assert_eq!(c.eval(0.6), 1.0);
    }

    #[test]
    fn curves_default_loads_via_toml_roundtrip() {
        // Empty [curves] section must deserialize to Curves::default() successfully,
        // because AiTuning uses #[serde(default)] — the game reads defaults from
        // Rust when the TOML key is absent, but this test verifies the path where
        // the TOML section is present but empty.
        let toml_src = "[curves]\n";
        let tuning: AiTuning = toml::from_str(toml_src).expect("empty [curves] must parse");
        let def = Curves::default();
        // Spot-check a few fields match defaults.
        assert_eq!(tuning.curves.self_preserve_dmg_alpha, def.self_preserve_dmg_alpha);
        // Spot-check curve evals at mid return 0.5.
        match (tuning.curves.self_preserve_hp, def.self_preserve_hp) {
            (ResponseCurve::Logistic { mid: a, k: ka }, ResponseCurve::Logistic { mid: b, k: kb }) => {
                assert_eq!(a, b);
                assert_eq!(ka, kb);
            }
            _ => panic!("self_preserve_hp should be Logistic"),
        }
    }
}
