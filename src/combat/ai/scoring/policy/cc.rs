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
/// Formula: `cc_turns × WEIGHT_CC + armor_shred × WEIGHT_SHRED`.
///
/// # Arguments
/// - `cc_turns` — total CC-denial value: projected damage denied via stun-class
///   status effects (Σ `horizon_window_sum` over stun applications). This is the
///   `stun_denial_value` contribution.
/// - `armor_shred` — HP-equivalent value of armor reduction statuses applied
///   (Σ `armor_bonus.abs() × duration`).
///
/// Consumer migration: step 4.10. Bit-identity with legacy `deny_value`
/// property-tested in 4.10.
pub fn value(cc_turns: f32, armor_shred: f32) -> f32 {
    const WEIGHT_CC: f32 = 1.0;
    const WEIGHT_SHRED: f32 = 1.0;
    cc_turns * WEIGHT_CC + armor_shred * WEIGHT_SHRED
}

#[cfg(test)]
mod tests {
    use super::*;

    // weights are placeholder 1.0; expand when non-trivial
    #[test]
    fn cc_value_zero_and_additive() {
        // Zero case.
        assert_eq!(value(0.0, 0.0), 0.0);
        // Additive: 10 + 3 = 13.
        assert!((value(10.0, 3.0) - 13.0).abs() < 1e-6);
        // Each component contributes independently.
        assert!((value(10.0, 0.0) - 10.0).abs() < 1e-6);
        assert!((value(0.0, 3.0) - 3.0).abs() < 1e-6);
        // Monotonic in each dimension.
        assert!(value(20.0, 3.0) > value(10.0, 3.0));
        assert!(value(10.0, 6.0) > value(10.0, 3.0));
    }
}
