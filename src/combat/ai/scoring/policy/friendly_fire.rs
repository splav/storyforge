//! Friendly-fire penalty policy — cost of hitting an ally with splash damage.

/// HP-equivalent cost of dealing `raw_dmg` (≥ 0) to a friendly unit:
/// `raw × (1 + raw / max_hp)`. The quadratic term super-penalises deleting a
/// high fraction of an ally's health bar (100→0 HP ≫ shaving 5 off a full bar).
pub fn penalty(raw_dmg: f32, max_hp: i32) -> f32 {
    raw_dmg * (1.0 + raw_dmg / max_hp.max(1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_raw_gives_zero() {
        assert_eq!(penalty(0.0, 100), 0.0);
    }

    #[test]
    fn small_hit_close_to_linear() {
        // raw=1, max_hp=100 → 1 × (1 + 1/100) = 1.01
        let v = penalty(1.0, 100);
        assert!((v - 1.01).abs() < 1e-6);
    }

    #[test]
    fn full_hp_obliteration_doubles() {
        // raw == max_hp → penalty = max_hp × (1 + 1) = 2 × max_hp
        let v = penalty(100.0, 100);
        assert!((v - 200.0).abs() < 1e-6);
    }

    #[test]
    fn monotonic_increasing_in_raw() {
        let max_hp = 100;
        assert!(penalty(20.0, max_hp) > penalty(10.0, max_hp));
        assert!(penalty(10.0, max_hp) > penalty(5.0, max_hp));
        assert!(penalty(5.0, max_hp) > penalty(0.0, max_hp));
    }

    #[test]
    fn super_linear_growth() {
        // penalty grows faster than linear: doubling raw > doubles penalty.
        let p1 = penalty(10.0, 100);
        let p2 = penalty(20.0, 100);
        assert!(p2 > 2.0 * p1);
    }
}
