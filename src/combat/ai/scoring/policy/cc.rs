//! CC composite value policy — combines turn denial, vulnerability, and armor
//! reduction. No consumers yet (migration + legacy-`deny_value` bit-identity
//! property tests land in step 4.10). Weights are 1.0 placeholders.

/// Combined HP-equivalent value of crowd-control effects applied in one action.
///
/// Formula: `cc_turns × WEIGHT_CC + armor_shred × WEIGHT_SHRED`, where
/// `cc_turns` is projected stun-denial damage (Σ `horizon_window_sum`) and
/// `armor_shred` is Σ `armor_bonus.abs() × duration`.
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
