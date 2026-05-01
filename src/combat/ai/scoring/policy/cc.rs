//! CC composite value policy — combined value of crowd-control effects.
//!
//! This module provides a single composite entry point that combines the three
//! orthogonal CC dimensions: turn denial, vulnerability, and armor reduction.
//! It has no consumers in step 4.7 — consumer migration happens in step 4.10.
//!
//! Weights are 1.0 for the initial scaffold. They will be validated for
//! bit-identity with the legacy `deny_value` path via property tests in 4.10.

/// Combined HP-equivalent value of crowd-control effects applied in one action.
///
/// Formula: `cc_turns × WEIGHT_CC + vulnerability × WEIGHT_VULN + armor_shred × WEIGHT_SHRED`.
///
/// # Arguments
/// - `cc_turns` — total CC-denial value: projected damage denied via stun-class
///   status effects (Σ `horizon_window_sum` over stun applications). This is the
///   `stun_denial_value` contribution.
/// - `vulnerability` — HP-equivalent value of vulnerability statuses applied
///   (Σ `damage_taken_bonus.abs() × duration`).
/// - `armor_shred` — HP-equivalent value of armor reduction statuses applied
///   (Σ `armor_bonus.abs() × duration`).
///
/// Consumer migration: step 4.10. Bit-identity with legacy `deny_value`
/// property-tested in 4.10.
pub fn value(cc_turns: f32, vulnerability: f32, armor_shred: f32) -> f32 {
    const WEIGHT_CC: f32 = 1.0;
    const WEIGHT_VULN: f32 = 1.0;
    const WEIGHT_SHRED: f32 = 1.0;
    cc_turns * WEIGHT_CC + vulnerability * WEIGHT_VULN + armor_shred * WEIGHT_SHRED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_gives_zero() {
        assert_eq!(value(0.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn additive_over_components() {
        let v = value(10.0, 5.0, 3.0);
        assert!((v - 18.0).abs() < 1e-6);
    }

    #[test]
    fn each_component_contributes_independently() {
        assert!((value(10.0, 0.0, 0.0) - 10.0).abs() < 1e-6);
        assert!((value(0.0, 5.0, 0.0) - 5.0).abs() < 1e-6);
        assert!((value(0.0, 0.0, 3.0) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn monotonic_in_each_component() {
        assert!(value(20.0, 5.0, 3.0) > value(10.0, 5.0, 3.0));
        assert!(value(10.0, 10.0, 3.0) > value(10.0, 5.0, 3.0));
        assert!(value(10.0, 5.0, 6.0) > value(10.0, 5.0, 3.0));
    }
}
