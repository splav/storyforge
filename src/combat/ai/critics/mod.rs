//! Critics layer — step 10.0 / 10.1 / 10.2.
//!
//! `PlanCritic` trait + associated types. Each critic evaluates a single plan
//! after scoring and returns an `Option<CriticHit>`:
//! - `None` = plan passes this critic (no action).
//! - `Some(hit)` = plan violates a heuristic; `hit.multiplier` is applied
//!   multiplicatively to `ann.score` by `CriticsStage`.
//!
//! Concrete critics are implemented in sub-modules (step 10.1-10.3); this file
//! defines only the shared contract and data types.

pub mod blindspot_ranged;
pub mod overcommit_into_danger;
pub mod self_lethal_without_payoff;

pub use blindspot_ranged::BlindspotRanged;
pub use overcommit_into_danger::{OvercommitIntoDanger, OvercommitSource};
pub use self_lethal_without_payoff::SelfLethalWithoutPayoff;

use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A single heuristic check applied to one plan after base scoring.
///
/// Implementors must be `Send + Sync` so that `CriticsStage` can hold a
/// `Vec<Box<dyn PlanCritic>>` without extra constraints.
pub trait PlanCritic: Send + Sync {
    /// Short identifier used in logs and debug output (e.g. `"overcommit_into_danger"`).
    fn name(&self) -> &'static str;

    /// Evaluate one plan. Returns `Some(hit)` when the critic fires, `None` otherwise.
    fn evaluate(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit>;
}

// ── CriticKind ────────────────────────────────────────────────────────────────

/// Identifies which critic produced a `CriticHit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticKind {
    /// Unit is low-HP or has high AoO exposure and moves into danger.
    OvercommitIntoDanger,
    /// Self-damage AoE cast with negligible payoff (kills / ally rescues).
    SelfLethalWithoutPayoff,
    /// Ranged unit ends its turn without line-of-sight to any enemy.
    BlindspotRanged,
    /// Buff/status cast on an ally who already has the same buff active.
    BuffIntoVoid,
    /// Expensive mana-cost ability with low expected impact.
    RareResourceForLowImpact,
    /// Heal cast on an ally with high HP who is not in danger.
    HealWithoutRescueValue,
}

// ── CriticReason ──────────────────────────────────────────────────────────────

/// Structured context explaining why a critic fired.
///
/// Each variant corresponds to one concrete critic. New variants are added in
/// steps 10.1–10.3; `#[serde(tag = "kind")]` ensures forward-compatible JSON.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriticReason {
    /// `OvercommitIntoDanger` fired — records which hazard signal dominated.
    OvercommitIntoDanger {
        /// Which of the two input signals produced the stronger penalty.
        source: overcommit_into_danger::OvercommitSource,
        /// The normalised risk ratio used to derive the multiplier:
        /// `surv` for SurvivalPath, `aoo_dmg / actor.hp` for AooBleed.
        ratio: f32,
    },
    /// `SelfLethalWithoutPayoff` fired — records the damage and payoff ratios.
    SelfLethalWithoutPayoff {
        /// `self_damage_total / actor.max_hp`.
        self_dmg_ratio: f32,
        /// Normalised payoff estimate (`payoff / actor.max_hp`).
        payoff_estimate: f32,
    },
    /// `BlindspotRanged` fired — ranged actor ends turn with no visible enemies.
    BlindspotRanged {
        /// Number of enemies visible from `final_pos`. Always 0 when the critic
        /// fires; kept as a field for observability in structured logs.
        enemies_visible: u32,
    },
}

// ── CriticHit ─────────────────────────────────────────────────────────────────

/// A single critic evaluation that fired for a plan.
///
/// `multiplier` is applied multiplicatively to `ann.score`
/// (`ann.score *= hit.multiplier`). Values < 1.0 penalise, values > 1.0 reward.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CriticHit {
    /// Which critic produced this hit.
    pub critic: CriticKind,
    /// Score multiplier to apply (< 1.0 = penalty, > 1.0 = bonus).
    pub multiplier: f32,
    /// Structured diagnostic context for this hit.
    pub reason: CriticReason,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::PlanAnnotation;

    #[test]
    fn plan_annotation_critics_default_empty() {
        assert!(
            PlanAnnotation::default().critics.is_empty(),
            "PlanAnnotation::default() must have an empty critics vec",
        );
    }

    #[test]
    fn critic_kind_serde_round_trip() {
        // Sanity-check that all variants survive JSON round-trip (snake_case naming).
        let kinds = [
            CriticKind::OvercommitIntoDanger,
            CriticKind::SelfLethalWithoutPayoff,
            CriticKind::BlindspotRanged,
            CriticKind::BuffIntoVoid,
            CriticKind::RareResourceForLowImpact,
            CriticKind::HealWithoutRescueValue,
        ];
        for k in kinds {
            let json = serde_json::to_string(&k).expect("serialize");
            let back: CriticKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(k, back);
        }
    }

    #[test]
    fn critic_reason_serde_round_trip() {
        use overcommit_into_danger::OvercommitSource;
        let reasons: Vec<CriticReason> = vec![
            CriticReason::OvercommitIntoDanger { source: OvercommitSource::SurvivalPath, ratio: 0.5 },
            CriticReason::OvercommitIntoDanger { source: OvercommitSource::AooBleed, ratio: 0.8 },
            CriticReason::SelfLethalWithoutPayoff { self_dmg_ratio: 0.45, payoff_estimate: 0.1 },
            CriticReason::BlindspotRanged { enemies_visible: 0 },
        ];
        for r in reasons {
            let json = serde_json::to_string(&r).expect("serialize");
            let back: CriticReason = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(r, back);
        }
    }
}
