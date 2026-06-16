//! Heal value policy — HP-equivalent value of healing applied to an ally.

/// HP-equivalent value of `restored_hp` healing delivered to a target.
///
/// Formula: `delta_pct × horizon_sum × urgency` where
/// - `delta_pct = restored_hp / target_max_hp` — fraction of HP bar restored.
/// - `horizon_sum` — sum of target's projected damage horizon (caller-provided,
///   already clamped to `target.threat` minimum).
/// - `urgency = 1.0 + hp_missing.max(incoming).min(1.0)` — multiplier ∈ [1, 2].
///
/// The urgency factor prioritises healing units in mortal danger: a heal on a
/// 5-HP target is worth twice the same heal on a 90-HP target.
///
/// `restored_hp` is assumed already clamped to missing HP; callers must return
/// `0.0` directly when nothing is missing rather than calling this.
pub fn value(
    restored_hp: f32,
    target_max_hp: i32,
    target_hp: i32,
    danger_at_target: f32,
    horizon_sum: f32,
) -> f32 {
    let delta_pct = restored_hp / target_max_hp.max(1) as f32;
    let hp_missing = 1.0 - (target_hp as f32 / target_max_hp.max(1) as f32);
    let incoming = (danger_at_target / target_hp.max(1) as f32).min(1.0);
    let urgency = 1.0 + hp_missing.max(incoming).min(1.0);
    delta_pct * horizon_sum * urgency
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_restored_gives_zero() {
        assert_eq!(value(0.0, 100, 50, 0.0, 10.0), 0.0);
    }

    #[test]
    fn full_hp_target_low_urgency() {
        // hp_missing = 0, incoming = 0 → urgency = 1.0
        let v = value(10.0, 100, 100, 0.0, 20.0);
        let expected = (10.0 / 100.0) * 20.0 * 1.0;
        assert!((v - expected).abs() < 1e-6);
    }

    #[test]
    fn near_death_target_high_urgency() {
        // hp = 10, max_hp = 100 → hp_missing = 0.9 → urgency = 1.9
        let v = value(10.0, 100, 10, 0.0, 20.0);
        let expected = (10.0 / 100.0) * 20.0 * 1.9;
        assert!((v - expected).abs() < 1e-6);
    }

    #[test]
    fn danger_drives_urgency_above_hp_missing() {
        // hp = 80, max_hp = 100 → hp_missing = 0.2
        // danger = 200, target_hp = 80 → incoming = min(200/80, 1.0) = 1.0
        // urgency = 1.0 + max(0.2, 1.0).min(1.0) = 2.0
        let v = value(10.0, 100, 80, 200.0, 20.0);
        let expected = (10.0 / 100.0) * 20.0 * 2.0;
        assert!((v - expected).abs() < 1e-6);
    }

    #[test]
    fn urgency_capped_at_two() {
        // Max urgency = 1.0 + 1.0 = 2.0
        let v = value(10.0, 100, 1, 9999.0, 20.0);
        let expected = (10.0 / 100.0) * 20.0 * 2.0;
        assert!((v - expected).abs() < 1e-6);
    }

    #[test]
    fn proportional_to_horizon_sum() {
        let v1 = value(10.0, 100, 50, 0.0, 10.0);
        let v2 = value(10.0, 100, 50, 0.0, 20.0);
        assert!((v2 - 2.0 * v1).abs() < 1e-6);
    }
}
