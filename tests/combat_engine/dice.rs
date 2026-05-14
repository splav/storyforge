//! Step 2 unit tests: `DiceSource` trait, `ExpectedValue`, `DiceRng`.
//!
//! Core assertion: `ExpectedValue::expected(d)` ≈ mean of 10 000 `DiceRng`
//! rolls within 1% of the true expected value.

use storyforge::combat_engine::dice::{DiceExpr, DiceRng, DiceSource, ExpectedValue};

fn monte_carlo_mean(expr: DiceExpr, n: u32, seed: u64) -> f32 {
    let mut rng = DiceRng::with_seed(seed);
    let total: i64 = (0..n).map(|_| rng.roll(&expr) as i64).sum();
    total as f32 / n as f32
}

/// For each dice expression the Monte Carlo mean over 10k rolls must be
/// within 1% of the analytical expected value.
#[test]
fn expected_matches_monte_carlo_within_1pct() {
    let cases = [
        DiceExpr::new(1, 6, 0),   // 1d6:   E=3.5
        DiceExpr::new(2, 6, 0),   // 2d6:   E=7.0
        DiceExpr::new(1, 20, 0),  // 1d20:  E=10.5
        DiceExpr::new(3, 8, 2),   // 3d8+2: E=15.5
        DiceExpr::new(0, 6, 5),   // bonus-only: E=5.0
    ];
    let ev = ExpectedValue;
    for expr in cases {
        let analytical = ev.expected(expr);
        let mc = monte_carlo_mean(expr, 10_000, 0xDEAD_BEEF);
        let tolerance = analytical.abs() * 0.01;
        assert!(
            (analytical - mc).abs() <= tolerance,
            "expr={expr:?}: analytical={analytical}, mc={mc}, diff={}",
            (analytical - mc).abs()
        );
    }
}

/// `ExpectedValue::roll()` returns `round(expected)` — deterministic, no RNG.
#[test]
fn expected_value_roll_is_deterministic() {
    let mut ev = ExpectedValue;
    let expr = DiceExpr::new(1, 6, 0); // E=3.5 → rounds to 4
    let r1 = ev.roll(expr);
    let r2 = ev.roll(expr);
    assert_eq!(r1, r2);
    assert_eq!(r1, 4); // round(3.5) = 4
}

/// `DiceRng` rolls are in range `[1, sides]` for each die.
#[test]
fn seeded_dice_rolls_in_range() {
    let mut rng = DiceRng::with_seed(42);
    let expr = DiceExpr::new(4, 6, 0); // 4d6
    for _ in 0..500 {
        let r = rng.roll(&expr);
        assert!((4..=24).contains(&r), "4d6 rolled {r}");
    }
}

/// Bonus-only expression (count=0) always returns the bonus.
#[test]
fn bonus_only_expression() {
    let mut rng = DiceRng::with_seed(99);
    let expr = DiceExpr::new(0, 6, 7);
    assert_eq!(rng.roll(&expr), 7);
    assert_eq!(ExpectedValue.expected(expr), 7.0);
}
