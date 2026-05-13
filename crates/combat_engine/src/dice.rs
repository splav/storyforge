//! `DiceSource` trait and its two implementations:
//!
//! - `SeededDice` — real path: LCG RNG, same algorithm as `core::DiceRng`.
//! - `ExpectedValue` — sim path: `roll()` returns the mathematical expected
//!   value (no mutation); `expected()` is trivially the same.
//!
//! Design decision 6.4: `DiceSource::roll(DiceExpr)` is the **only** RNG
//! entry point in the engine.  No implicit advances.

/// A dice expression `NdS + bonus`.
///
/// Shape mirrors `crate::core::DiceExpr` so conversion is trivial, but the
/// engine owns this copy to stay free of any `crate::core` dep that might
/// transitively pull in Bevy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiceExpr {
    pub count: u32,
    pub sides: u32,
    pub bonus: i32,
}

impl DiceExpr {
    pub fn new(count: u32, sides: u32, bonus: i32) -> Self {
        Self { count, sides, bonus }
    }

    /// Analytical expected value: `N*(S+1)/2 + bonus`.
    pub fn expected(self) -> f32 {
        self.count as f32 * (self.sides as f32 + 1.0) / 2.0 + self.bonus as f32
    }
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// RNG abstraction injected into `step()`.
///
/// - Real path: `SeededDice` (or the `core::DiceRng` adapter in the bridge).
/// - Sim path: `ExpectedValue` — stateless, no random draws.
pub trait DiceSource {
    /// Draw a random result for `dice`.  Real path advances the RNG.
    fn roll(&mut self, dice: DiceExpr) -> i32;
    /// Return the analytical expected value without advancing the RNG.
    fn expected(&self, dice: DiceExpr) -> f32;
}

// ── ExpectedValue ─────────────────────────────────────────────────────────────

/// Sim-path dice source.  `roll()` returns `round(expected)` so the sim gets
/// deterministic, average-case results without touching any RNG state.
#[derive(Default, Clone)]
pub struct ExpectedValue;

impl DiceSource for ExpectedValue {
    fn roll(&mut self, dice: DiceExpr) -> i32 {
        dice.expected().round() as i32
    }

    fn expected(&self, dice: DiceExpr) -> f32 {
        dice.expected()
    }
}

// ── SeededDice ────────────────────────────────────────────────────────────────

/// Real-path dice source — LCG identical to `core::DiceRng`.
///
/// Used by tests (and eventually by the Bevy bridge via the `DiceRngAdapter`
/// wrapper that delegates to `core::DiceRng`).
pub struct SeededDice {
    state: u64,
}

impl SeededDice {
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

    fn roll_d(&mut self, sides: u32) -> i32 {
        assert!(sides >= 1);
        ((self.next_u64() % sides as u64) as i32) + 1
    }
}

impl DiceSource for SeededDice {
    fn roll(&mut self, dice: DiceExpr) -> i32 {
        let mut total = dice.bonus;
        for _ in 0..dice.count {
            total += self.roll_d(dice.sides);
        }
        total
    }

    fn expected(&self, dice: DiceExpr) -> f32 {
        dice.expected()
    }
}
