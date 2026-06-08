//! Generic factor registry: shared types and the minimal `factor_kind!` macro.
//!
//! `factor_kind!` generates only the enum definition and the `COUNT` constant.
//! All methods (`name`, `signed`, `compute`, `iter`, `from_name`,
//! `need_modulation`, `normalize`) are written as explicit `impl` blocks in
//! each `{step,plan,terminal}/mod.rs`. This is intentional: the methods need
//! different signatures per factor kind, and `macro_rules!` cannot dispatch
//! over meta-variables at mixed repetition depths.

use crate::combat::ai::appraisal::NeedSignals;

/// Per-factor batch statistics collected during one `finalize_scores` pass.
/// Used by `default_norm` to normalise raw values to `[−1, 1]` or `[0, 1]`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BatchStats {
    pub min: f32,
    pub max: f32,
}

/// Which `NeedSignals` axis amplifies a terminal factor.
///
/// `None` → multiplier is always 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedAxis {
    SelfPreserve,
    FinishTarget,
    RescueAlly,
    Reposition,
    SetupAOE,
    None,
}

impl NeedAxis {
    /// Amplification multiplier: `1.0 + signal` for named axes, `1.0` for `None`.
    pub fn amplify(self, n: &NeedSignals) -> f32 {
        match self {
            Self::SelfPreserve => 1.0 + n.self_preserve,
            Self::FinishTarget => 1.0 + n.finish_target,
            Self::RescueAlly => 1.0 + n.rescue_ally,
            Self::Reposition => 1.0 + n.reposition,
            Self::SetupAOE => 1.0 + n.setup_aoe,
            Self::None => 1.0,
        }
    }
}

/// Default batch normalisation.
///
/// Signed factors: divide by `max(|min|, max)` → `[−1, 1]`.
/// Unsigned factors: divide by `max` → `[0, 1]`.
/// Returns `0.0` when the denominator is ≤ `f32::EPSILON`.
pub fn default_norm(raw: f32, batch: &BatchStats, signed: bool) -> f32 {
    let denom = if signed {
        batch.min.abs().max(batch.max.abs())
    } else {
        batch.max
    };
    if denom > f32::EPSILON {
        raw / denom
    } else {
        0.0
    }
}

// ── factor_kind! macro ────────────────────────────────────────────────────────
//
// Minimal macro: generates the enum and a `COUNT` const.
// Everything else is written by hand in the calling module's `impl` block.

/// Generate a factor enum and its variant count.
///
/// # Syntax
/// ```ignore
/// factor_kind! {
///     name: MyFactor,
///     variants: [ Variant1, Variant2, Variant3 ]
/// }
/// ```
///
/// Produces:
/// - `#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)] pub enum MyFactor { ... }`
/// - `pub const COUNT: usize = N;`
#[macro_export]
macro_rules! factor_kind {
    (
        name: $Enum:ident,
        variants: [ $( $Variant:ident ),+ $(,)? ]
        $(,)?
    ) => {
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
        pub enum $Enum {
            $( $Variant, )+
        }

        /// Variant count.
        pub const COUNT: usize = [$( stringify!($Variant), )+].len();
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_norm_unsigned() {
        let batch = BatchStats { min: 0.0, max: 4.0 };
        assert!((default_norm(2.0, &batch, false) - 0.5).abs() < f32::EPSILON);
        assert_eq!(default_norm(0.0, &batch, false), 0.0);
    }

    #[test]
    fn default_norm_signed_uses_abs_max() {
        let batch = BatchStats {
            min: -3.0,
            max: 2.0,
        };
        // denom = 3.0
        assert!((default_norm(-3.0, &batch, true) - -1.0).abs() < f32::EPSILON);
        assert!((default_norm(2.0, &batch, true) - (2.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn default_norm_zero_denom_returns_zero() {
        let batch = BatchStats { min: 0.0, max: 0.0 };
        assert_eq!(default_norm(1.0, &batch, false), 0.0);
        assert_eq!(default_norm(1.0, &batch, true), 0.0);
    }

    #[test]
    fn need_axis_none_always_one() {
        let n = NeedSignals::default();
        assert_eq!(NeedAxis::None.amplify(&n), 1.0);
    }
}
