//! `DiceSource` trait and its two implementations:
//!
//! - `DiceRng` — real path: LCG RNG with scripted-rolls support.
//! - `ExpectedValue` — sim path: `roll()` returns the mathematical expected
//!   value (no mutation); `expected()` is trivially the same.
//!
//! Design decision 6.4: `DiceSource::roll(DiceExpr)` is the **only** RNG
//! entry point in the engine.  No implicit advances.

/// A dice expression `NdS + bonus`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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
    pub fn expected_disadvantage(self) -> f32 {
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

// ── Trait ────────────────────────────────────────────────────────────────────

/// RNG abstraction injected into `step()`.
///
/// - Real path: `DiceRng` (canonical LCG with scripted-rolls).
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

// ── DiceRng ───────────────────────────────────────────────────────────────────

/// Canonical real-path dice source — LCG with scripted-rolls support.
///
/// For testing: push scripted results via `script()`. While the queue is
/// non-empty, `roll_d` pops from the front instead of using the LCG.
pub struct DiceRng {
    state: u64,
    scripted: std::collections::VecDeque<i32>,
}

impl Default for DiceRng {
    fn default() -> Self {
        Self { state: 0xDEAD_BEEF_CAFE_1337, scripted: std::collections::VecDeque::new() }
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
        let label = format!("{}d{}", expr.count, expr.sides);
        let min = a.min(b);
        (min, format!("{label}={a} vs {label}={b} помеха={min}"))
    }
}

impl DiceSource for DiceRng {
    fn roll(&mut self, dice: DiceExpr) -> i32 {
        Self::roll(self, &dice)
    }

    fn expected(&self, dice: DiceExpr) -> f32 {
        dice.expected()
    }
}
