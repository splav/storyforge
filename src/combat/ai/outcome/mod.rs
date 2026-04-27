//! ActionOutcomeEstimate — facts about what a plan step did.
//!
//! ## Contract
//!
//! Outcome contains **facts only** — raw numerical signals about the step's
//! effect on the board. No policy weighting, no value judgment, no progression
//! curves. Any `× progress` / `× urgency` / `× horizon` / `× (1 + raw/max_hp)` —
//! that's policy, lives in `combat::ai::policy`.
//!
//! ## Layered model
//!
//! ```text
//! sim::StepOutcome  →  outcome::builder  →  ActionOutcomeEstimate  →  policy + factors  →  score
//! (raw mechanics)     (structures facts)   (fact vector)             (judgment)            (number)
//! ```
//!
//! ## Invariants
//!
//! 1. Outcome population (in builder) MUST NOT call any function from `policy::*`.
//!    If you need to derive a value, derive it from raw mechanics, not from policy.
//! 2. Policy formulas MUST be pure functions of (outcome, target, caster).
//!    No state, no side effects, no caching beyond the call.
//! 3. Outcome MUST be the same shape for Cast and Move steps. Move-specific facts
//!    are 0 for Cast, vice versa.
//! 4. New mechanics extend outcome by adding fact fields. Do not add policy fields.
//!
//! ## Consumers (authoritative list, finalised in step 4.13)
//!
//! ### Active fact readers
//! - `factors::offensive::compute_offensive` — primary scoring consumer; applies all
//!   damage / heal / cc policies to outcome facts.
//! - `planning::terminal::compute_secure_kill` — reads `p_kill_now` / `p_kill_soon`.
//! - `repair::goal::extract_goal_context` — reads `p_kill_now` for Finish/Pressure classification.
//! - `planning::future_value::λ_attack` (`attack_component_intent`) — reads outcome from
//!   hypothetical path; applies `policy::damage::value` for intent score.
//! - `planning::picker::record_committed_reservations` — reads `outcome.enemy_damage`
//!   directly (raw fact for reservation bookkeeping).
//!
//! ### Non-consumers (NOT applicable, not a bug)
//! - `trade::*` — actor valuation, not action outcome.
//! - `terminal::compute_*` (except `secure_kill`) — end-state metrics from snapshot/maps.
//! - `intent_score` non-Cast branches (Reposition / ProtectAlly / SetupAOE / LastStand) —
//!   position/ability-type logic, outcome not applicable.

pub mod builder;

// Re-export builder items so existing callers can import them via
// `crate::combat::ai::outcome::*` without changing their import paths.
pub use builder::{
    estimate_kill_soon,
    from_sim_step,
    hypothetical,
    step_path_danger,
};

use crate::combat::ai::factors::{PlanFactorValues, FactorTerminalScore};
use serde::{Deserialize, Serialize};

/// Structured estimate of a single plan step's consequences.
///
/// Contains **facts only** — raw, policy-free measurements derived from the sim
/// step or the ability def. Consumers apply policy formulas from
/// `combat::ai::policy::*` to derive HP-equivalent scores. No policy weighting
/// lives here; see module-level doc for the full contract and consumer list.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    // ── Damage facts (raw, populated by sim walk or hypothetical) ──

    /// Raw damage dealt to all enemies (sum over AoE area); 0 for Move steps.
    pub enemy_damage: f32,
    /// Per-entity enemy damage breakdown. Empty for single-target casts (use
    /// `enemy_damage`); populated for AoE casts. Enables step-10 critics.
    #[serde(default)]
    pub enemy_damage_per_entity: Vec<(bevy::prelude::Entity, f32)>,
    /// Raw damage to allies (AoE friendly fire); 0 for single-target / Move.
    pub ally_damage: f32,
    /// Per-entity ally damage breakdown. Empty for single-target / Move.
    #[serde(default)]
    pub ally_damage_per_entity: Vec<(bevy::prelude::Entity, f32)>,
    /// Raw damage to the actor (AoE self-hit, lifesteal cost); 0 otherwise.
    pub self_damage: f32,

    // ── Kill facts ──

    /// 1.0 if this step killed ≥1 enemy this turn, else 0.0.
    /// Float reserved for forward-compat (probabilistic AI with dice variance).
    pub p_kill_now: f32,
    /// 1.0 if direct + DoT will kill within the damage horizon, else 0.0.
    pub p_kill_soon: f32,

    // ── Status / control facts (aggregated; per-status breakdown — backlog) ──

    /// Σ (skips_turn × duration_rounds) over enemies hit by this step.
    pub cc_turns_applied: f32,
    /// Σ (damage_taken_bonus × duration_rounds) over enemies hit.
    pub vulnerability_applied: f32,
    /// Σ (armor_bonus × duration_rounds) over enemies hit (negative = shred).
    pub armor_shred_applied: f32,

    // ── Support facts ──

    /// Raw HP restored, clamped to the target's missing HP; 0 for non-heal.
    pub hp_restored: f32,

    // ── Movement facts (Move steps; 0 for Cast) ──

    /// Worst danger value along the Move path (max over path tiles).
    pub path_max_danger: f32,
    /// Movement points consumed by this Move step.
    pub mp_spent: i32,

    // ── Resource facts ──

    /// Action points spent by this step.
    pub ap_spent: i32,
    /// Mana spent by this step.
    pub mana_spent: i32,
    /// Rage spent by this step.
    pub rage_spent: i32,
    /// Other resource costs (Energy and any future kinds).
    pub other_resource_spent: i32,

}

/// Result of the viability-gate pass for one plan (step 7.1).
///
/// `passed = true` means the intent signal for this plan met the threshold and
/// no swap was triggered (or no threshold applies). `adjusted_score` is the
/// final score after any intent-column rewrite that viability triggered; it
/// equals the pre-viability score when no swap occurred.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ViabilityResult {
    /// Whether the viability gate passed without triggering a fallback swap.
    pub passed: bool,
    /// Score after viability rewrite (equals pre-viability score when passed).
    pub adjusted_score: f32,
}

impl Default for ViabilityResult {
    fn default() -> Self {
        Self { passed: true, adjusted_score: 0.0 }
    }
}

/// Per-plan annotation bundle. Grows as pipeline stages accrue data
/// (outcome in wave 1; critics / band / agenda in later waves).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlanAnnotation {
    /// One ActionOutcomeEstimate per plan step, same length as TurnPlan.steps
    /// and TurnPlan.outcomes.
    #[serde(default)]
    pub outcomes: Vec<ActionOutcomeEstimate>,
    /// One-shot terminal-state evaluation. Populated by `terminal_state_score`
    /// in `finalize_scores`; consumed by aggregation in 5.4.
    /// Serialized into JSONL as of schema v29 as a named map.
    #[serde(default)]
    pub terminal: FactorTerminalScore,
    /// Step 6.2: repair affinity of this plan against the stored goal context.
    /// Populated in `pick_action` when `AiMemory.last_goal` is present.
    /// Default zero-filled when no stored goal exists. Consumed by the
    /// repair bonus aggregation in 6.3 (not read into score in 6.2).
    #[serde(default)]
    pub repair_affinity: crate::combat::ai::repair::RepairAffinity,
    /// Step 7.1: viability gate result for this plan.
    /// Default (passed=true, adjusted_score=0.0) when ViabilityStage has not
    /// run yet or the gate did not apply to this intent.
    #[serde(default)]
    pub viability: ViabilityResult,
    /// Step 7.1: sanity hits applied to this plan (rule + multiplier pairs).
    /// Empty until SanityStage runs or when no rules fired.
    #[serde(default)]
    pub sanity: Vec<crate::combat::ai::planning::sanity::SanityHit>,
    /// Step 7.2: adaptation decision for this plan (was PlanRanking.adaptation.reasons[i]).
    /// `None` when no adaptation trigger fired for this plan.
    #[serde(default)]
    pub adaptation: Option<AdaptationData>,
    /// Step 7.2: contract mask applied to this plan (ProtectSelf or KillableGate masking).
    /// `None` when no mask applied.
    #[serde(default)]
    pub contract: Option<ContractMaskHit>,
    /// Step 7.4: final aggregated score for this plan after all pipeline stages.
    /// Default 0.0. Written by scoring stages (replaces ScoredPool.scored).
    ///
    /// Serde wrapped because contract masks (ProtectSelf, KillableGate) set
    /// score = `f32::NEG_INFINITY` to sentinel-mask plans. JSON cannot represent
    /// non-finite floats; serde_json writes them as `null` and then fails to
    /// read back. The `f32_finite` adapter maps NEG_INFINITY → `f32::MIN`
    /// (-3.4e38) on write; on read accepts both finite numbers and `null`
    /// (decoded as `f32::MIN`). Production semantics preserved — runtime never
    /// round-trips score through JSON.
    #[serde(default, with = "crate::combat::ai::serde_helpers::f32_finite")]
    pub score: f32,
    /// Step 7.4: factor decomposition for this plan (v29 named map).
    /// Written by the initial scoring pass. Default PlanFactorValues::default().
    #[serde(default)]
    pub factors: PlanFactorValues,
    /// Step 7.4: whether this plan was chosen as the winning plan.
    /// Set to `true` by `PickBestStage`. Default false.
    #[serde(default)]
    pub chosen: bool,
    /// Step 7.4: pick mechanics info for the chosen plan.
    /// `None` for non-chosen plans. Set by `PickBestStage`.
    #[serde(default)]
    pub pick: Option<PickInfo>,
}

/// Adaptation reason + original (pre-adaptation) score for a single plan.
/// Written by `AdaptationStage`; consumed by the finalizer to build
/// `IntentReason::Adapted` for the winning plan.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdaptationData {
    pub reason: crate::combat::ai::planning::AdaptationReason,
    /// Score this plan had immediately before adaptation rescored it.
    pub original_score: f32,
}

/// Record of a contract mask hit (ProtectSelf or KillableGate).
/// Written by `ProtectSelfMaskStage` / `KillableGateStage`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContractMaskHit {
    /// Which mask applied: `"protect_self"` or `"killable_gate"`.
    pub mask: String,
    /// Score this plan had immediately before the mask set it to -∞.
    pub original_score: f32,
}

/// Pick diagnostics for the winning plan.
/// Written by `PickBestStage`; `None` on all non-chosen plans.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PickInfo {
    /// Top-K window, mercy flag, chosen position in the ranked pool.
    pub mechanics: crate::combat::ai::planning::PickMechanics,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All 17 fact fields default to zero / empty.
    #[test]
    fn default_outcome_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.enemy_damage, 0.0);
        assert!(o.enemy_damage_per_entity.is_empty());
        assert_eq!(o.ally_damage, 0.0);
        assert!(o.ally_damage_per_entity.is_empty());
        assert_eq!(o.self_damage, 0.0);
        assert_eq!(o.p_kill_now, 0.0);
        assert_eq!(o.p_kill_soon, 0.0);
        assert_eq!(o.cc_turns_applied, 0.0);
        assert_eq!(o.vulnerability_applied, 0.0);
        assert_eq!(o.armor_shred_applied, 0.0);
        assert_eq!(o.hp_restored, 0.0);
        assert_eq!(o.path_max_danger, 0.0);
        assert_eq!(o.mp_spent, 0);
        assert_eq!(o.ap_spent, 0);
        assert_eq!(o.mana_spent, 0);
        assert_eq!(o.rage_spent, 0);
        assert_eq!(o.other_resource_spent, 0);
    }

    #[test]
    fn default_annotation_is_empty() {
        let a = PlanAnnotation::default();
        assert!(a.outcomes.is_empty());
    }
}
