//! Per-step factor enum and per-variant leaf modules.
//!
//! `StepFactor` covers 7 per-step axes: damage, kill_now, kill_promised, cc,
//! heal, scarcity, saturation. Array slots 0..7 in `PlanFactorValues`.
//!
//! Signed factors (can be negative): `Scarcity`, `Saturation`.

pub mod cc;
pub mod damage;
pub mod heal;
pub mod kill_now;
pub mod kill_promised;
pub mod saturation;
pub mod scarcity;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::factors::registry::{default_norm, BatchStats};
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::utility::ScoringCtx;

crate::factor_kind! {
    name: StepFactor,
    variants: [
        Damage,
        KillNow,
        KillPromised,
        Cc,
        Heal,
        Scarcity,
        Saturation,
    ]
}

impl StepFactor {
    /// String name used in serde named maps and `from_name`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Damage       => "damage",
            Self::KillNow      => "kill_now",
            Self::KillPromised => "kill_promised",
            Self::Cc           => "cc",
            Self::Heal         => "heal",
            Self::Scarcity     => "scarcity",
            Self::Saturation   => "saturation",
        }
    }

    /// True for factors that can be negative (use symmetric normalisation).
    pub fn signed(self) -> bool {
        matches!(self, Self::Scarcity | Self::Saturation)
    }

    /// Normalise `raw` against `batch` using this factor's sign policy.
    pub fn normalize(self, raw: f32, batch: &BatchStats) -> f32 {
        default_norm(raw, batch, self.signed())
    }

    /// Variant count (same as `COUNT`).
    pub fn count() -> usize { COUNT }

    /// Iterator over all variants in declaration order.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Damage,
            Self::KillNow,
            Self::KillPromised,
            Self::Cc,
            Self::Heal,
            Self::Scarcity,
            Self::Saturation,
        ]
        .into_iter()
    }

    /// Look up a variant by its string name. Returns `None` for unknown names.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "damage"        => Some(Self::Damage),
            "kill_now"      => Some(Self::KillNow),
            "kill_promised" => Some(Self::KillPromised),
            "cc"            => Some(Self::Cc),
            "heal"          => Some(Self::Heal),
            "scarcity"      => Some(Self::Scarcity),
            "saturation"    => Some(Self::Saturation),
            _ => None,
        }
    }

    /// Compute this factor for a single scored step.
    ///
    /// `ctx.snap` must be the **pre-step** snapshot (caller applies
    /// `ctx.with_perspective(&sim_actor, pre_snap)` before entering the step
    /// loop). `needs` is forwarded for future step-11 use; current bodies
    /// ignore it.
    pub fn compute(
        self,
        ctx: &ScoringCtx,
        step: &ScoredStep,
        outcome: &ActionOutcomeEstimate,
        needs: &NeedSignals,
    ) -> f32 {
        match self {
            Self::Damage       => damage::compute(ctx, step, outcome, needs),
            Self::KillNow      => kill_now::compute(ctx, step, outcome, needs),
            Self::KillPromised => kill_promised::compute(ctx, step, outcome, needs),
            Self::Cc           => cc::compute(ctx, step, outcome, needs),
            Self::Heal         => heal::compute(ctx, step, outcome, needs),
            Self::Scarcity     => scarcity::compute(ctx, step, outcome, needs),
            Self::Saturation   => saturation::compute(ctx, step, outcome, needs),
        }
    }
}
