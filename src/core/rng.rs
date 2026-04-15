use bevy::prelude::*;

/// Simple LCG-based dice roller — no external rand dependency needed for a skeleton.
#[derive(Resource)]
pub struct DiceRng {
    state: u64,
}

impl Default for DiceRng {
    fn default() -> Self {
        Self {
            state: 0xDEAD_BEEF_CAFE_1337,
        }
    }
}

impl DiceRng {
    pub fn with_seed(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    pub fn roll_d(&mut self, sides: u32) -> i32 {
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

#[derive(Debug, Clone)]
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
            assert!(r >= 5 && r <= 20, "3d6+2 rolled {r}");
        }
    }
}
