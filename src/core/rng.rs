use bevy::prelude::*;

/// Simple LCG-based dice roller — no external rand dependency needed for a skeleton.
/// For testing: push scripted results via `script()`. While the queue is non-empty,
/// `roll_d` pops from the front instead of using the LCG.
#[derive(Resource)]
pub struct DiceRng {
    state: u64,
    scripted: std::collections::VecDeque<i32>,
}

impl Default for DiceRng {
    fn default() -> Self {
        Self {
            state: 0xDEAD_BEEF_CAFE_1337,
            scripted: std::collections::VecDeque::new(),
        }
    }
}

impl DiceRng {
    pub fn with_seed(seed: u64) -> Self {
        Self { state: seed, scripted: std::collections::VecDeque::new() }
    }

    /// Queue scripted roll results. While non-empty, `roll_d` pops from here.
    pub fn script(&mut self, results: &[i32]) {
        self.scripted.extend(results);
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    pub fn roll_d(&mut self, sides: u32) -> i32 {
        if let Some(v) = self.scripted.pop_front() {
            return v;
        }
        assert!(sides >= 1);
        ((self.next_u64() % sides as u64) as i32) + 1
    }

    pub fn roll(&mut self, expr: &DiceExpr) -> i32 {
        let mut total = expr.bonus;
        for _ in 0..expr.count {
            total += self.roll_d(expr.sides);
        }
        total
    }

    /// Rolls only the dice part of `expr` (ignores `expr.bonus`).
    /// Returns `(total, "NdS=X")` for use in breakdown strings.
    pub fn roll_dice(&mut self, expr: &DiceExpr) -> (i32, String) {
        let mut total = 0i32;
        for _ in 0..expr.count {
            total += self.roll_d(expr.sides);
        }
        (total, format!("{}d{}={}", expr.count, expr.sides, total))
    }

    /// Rolls dice twice and takes the lower result (disadvantage, D&D-style).
    /// Returns `(min_total, "NdS=A vs NdS=B помеха=min")`.
    pub fn roll_dice_disadvantage(&mut self, expr: &DiceExpr) -> (i32, String) {
        let (a, _) = self.roll_dice(expr);
        let (b, _) = self.roll_dice(expr);
        let dice_label = format!("{}d{}", expr.count, expr.sides);
        let min = a.min(b);
        (min, format!("{dice_label}={a} vs {dice_label}={b} помеха={min}"))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiceExpr {
    pub count: u32,
    pub sides: u32,
    pub bonus: i32,
}

impl DiceExpr {
    pub fn new(count: u32, sides: u32, bonus: i32) -> Self {
        Self { count, sides, bonus }
    }

    /// Expected value: E[NdS + bonus] = N*(S+1)/2 + bonus.
    pub fn expected(&self) -> f32 {
        self.count as f32 * (self.sides as f32 + 1.0) / 2.0 + self.bonus as f32
    }

    /// Expected value under disadvantage, **per-die** semantics: each die
    /// is rolled twice, the lower of the two values kept, then summed.
    /// Closed form: `N · (S+1)(2S+1) / (6S) + bonus`.
    ///
    /// **Caveat — divergence with the live resolver.** Right now
    /// `roll_dice_disadvantage` (live path) computes per-sum disadvantage:
    /// the entire `NdS` is rolled twice and the lower **total** kept. For
    /// `N > 1` per-sum is mathematically a softer discount than per-die
    /// (e.g. 2d6: per-sum E≈5.63, per-die E=5.06). Until the live mechanic
    /// is reconciled, AI scoring under-estimates damage on disadvantage
    /// casts of multi-die abilities by ~10%. Single-die abilities
    /// (the common case) match exactly. Tracked as a follow-up; for
    /// scoring purposes the directional signal — "disadvantage hurts" —
    /// is right either way.
    pub fn expected_disadvantage(&self) -> f32 {
        let n = self.count as f32;
        let k = self.sides as f32;
        if n <= 0.0 || k <= 0.0 {
            return self.bonus as f32;
        }
        // E[min of 2 rolls of 1dK] = (K+1)(2K+1) / (6K)
        let single_die_min = (k + 1.0) * (2.0 * k + 1.0) / (6.0 * k);
        n * single_die_min + self.bonus as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roll_d_always_in_range() {
        let mut rng = DiceRng::with_seed(12345);
        for sides in [4u32, 6, 8, 10, 12, 20] {
            for _ in 0..200 {
                let r = rng.roll_d(sides);
                assert!(r >= 1 && r <= sides as i32, "d{sides} rolled {r}");
            }
        }
    }

    #[test]
    fn roll_applies_bonus() {
        let mut rng = DiceRng::with_seed(42);
        let expr = DiceExpr::new(0, 6, 5); // 0 dice + 5 bonus = always 5
        assert_eq!(rng.roll(&expr), 5);
    }

    #[test]
    fn roll_sum_is_at_least_count_plus_bonus() {
        let mut rng = DiceRng::with_seed(99);
        let expr = DiceExpr::new(3, 6, 2); // 3d6+2: min=5, max=20
        for _ in 0..100 {
            let r = rng.roll(&expr);
            assert!((5..=20).contains(&r), "3d6+2 rolled {r}");
        }
    }

    /// Pin per-die semantics — for 2d6 per-die gives ≈5.06 while per-sum
    /// (`min` of two 2d6 totals) gives ≈5.63. Regressing to per-sum would
    /// silently shift AI damage estimates; this asserts we stay per-die.
    /// Single-die formula is a tautology (1d6 = 91/36 by both sides of the
    /// equation) — not worth asserting separately.
    #[test]
    fn expected_disadvantage_is_per_die_not_per_sum() {
        let two_d6 = DiceExpr::new(2, 6, 4);
        // per-die: 2 × 91/36 + 4 ≈ 9.056. per-sum would be ≈9.63.
        let got = two_d6.expected_disadvantage();
        assert!(
            (got - (91.0 / 18.0 + 4.0)).abs() < 0.01,
            "per-die 2d6+4 ≈ 9.056, got {got}",
        );
        assert!(got < two_d6.expected(), "disadv must be strictly less than normal");
    }
}
