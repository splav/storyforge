//! Per-plan factor enum and leaf modules.
//!
//! `PlanFactor` covers 3 plan-level axes: intent, tempo_gain, self_survival.
//! Array slots 7..10 in `PlanFactorValues` (after 7 StepFactor slots).
//!
//! Signed factors (can be negative): `Intent`, `TempoGain`.

pub mod intent;
pub mod self_survival;
pub mod tempo_gain;

use crate::combat::ai::scoring::factors::registry::{default_norm, BatchStats};
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

crate::factor_kind! {
    name: PlanFactor,
    variants: [
        Intent,
        TempoGain,
        SelfSurvival,
    ]
}

impl PlanFactor {
    /// String name used in serde named maps and `from_name`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Intent       => "intent",
            Self::TempoGain    => "tempo_gain",
            Self::SelfSurvival => "self_survival",
        }
    }

    /// True for factors that can be negative.
    pub fn signed(self) -> bool {
        matches!(self, Self::Intent | Self::TempoGain)
    }

    pub fn normalize(self, raw: f32, batch: &BatchStats) -> f32 {
        default_norm(raw, batch, self.signed())
    }

    pub fn count() -> usize { COUNT }

    pub fn iter() -> impl Iterator<Item = Self> {
        [Self::Intent, Self::TempoGain, Self::SelfSurvival].into_iter()
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "intent"        => Some(Self::Intent),
            "tempo_gain"    => Some(Self::TempoGain),
            "self_survival" => Some(Self::SelfSurvival),
            _ => None,
        }
    }

    /// Compute this plan-level factor.
    pub fn compute(self, plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx) -> f32 {
        match self {
            Self::Intent       => intent::compute(plan, intent, ctx),
            Self::TempoGain    => tempo_gain::compute(plan, intent, ctx),
            Self::SelfSurvival => self_survival::compute(plan, intent, ctx),
        }
    }
}
