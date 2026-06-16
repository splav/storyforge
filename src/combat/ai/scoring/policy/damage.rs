//! Damage value policy — HP-equivalent value of raw damage dealt.

/// HP-equivalent value of `raw` damage dealt to a target.
///
/// Formula: `raw × (0.5 + 0.5 × progress)` where
/// `progress = (raw / target_hp).min(1.0)`.
///
/// The progressive factor rewards finishing blows, so "finish them" plans
/// outcompete "chip them" plans at equal raw damage.
///
/// # Arguments
/// - `raw` — net expected damage after armor (≥ 0).
/// - `target_hp_pct_inv` — fraction of the target's current HP that `raw`
///   represents: `(raw / target.hp.max(1) as f32).min(1.0)`.
pub fn value(raw: f32, target_hp_pct_inv: f32) -> f32 {
    raw * (0.5 + 0.5 * target_hp_pct_inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_raw_gives_zero() {
        assert_eq!(value(0.0, 0.0), 0.0);
        assert_eq!(value(0.0, 1.0), 0.0);
    }

    #[test]
    fn full_progress_doubles_weight() {
        // At progress=1.0 the weight is (0.5 + 0.5) = 1.0, so value == raw.
        assert!((value(10.0, 1.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn zero_progress_halves_weight() {
        // At progress=0.0 the weight is 0.5, so value == raw * 0.5.
        assert!((value(10.0, 0.0) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn monotonic_in_raw_for_fixed_progress() {
        // Larger raw always produces larger value.
        let progress = 0.3;
        assert!(value(20.0, progress) > value(10.0, progress));
        assert!(value(10.0, progress) > value(5.0, progress));
    }

    #[test]
    fn progress_clamped_to_one_by_caller() {
        // value itself doesn't clamp — caller is responsible.
        // Just verify the formula at boundary.
        assert!((value(5.0, 1.0) - 5.0).abs() < 1e-6);
    }
}
