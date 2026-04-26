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
//! ## Consumers (authoritative list as of step 4.11)
//!
//! ### Active fact readers
//! - `factors::offensive::compute_offensive` — primary scoring consumer (4.10).
//! - `planning::terminal::compute_secure_kill` — reads `p_kill_now` / `p_kill_soon`.
//! - `repair::goal::extract_goal_context` — reads `p_kill_now` for Finish/Pressure classification.
//! - `planning::future_value::λ_attack` (`attack_component_intent`) — reads `expected_damage`
//!   from hypothetical path; migrates to `policy::damage::value` in 4.12.
//! - `planning::picker::record_committed_reservations` — reads `expected_damage`
//!   from hypothetical path; migrates to `policy::damage::value` in 4.12.
//!
//! ### Non-consumers (NOT applicable, not a bug)
//! - `trade::*` — actor valuation, not action outcome.
//! - `terminal::compute_*` (except `secure_kill`) — end-state metrics from snapshot/maps.
//! - `intent_score` non-Cast branches (Reposition / ProtectAlly / SetupAOE / LastStand) —
//!   position/ability-type logic, outcome not applicable.
//!
//! ## Legacy fields
//!
//! Steps 4.8 added new fact fields alongside pre-4.8 policy-baked legacy fields.
//! Legacy fields are `#[deprecated]` and drop in 4.12 together with the schema bump
//! v27 → v28. See docs/ai_rework_step4_plan.md §4.12.

pub mod builder;

// Re-export builder items so existing callers can import them via
// `crate::combat::ai::outcome::*` without changing their import paths.
//
// Public API (was `pub` in old outcome.rs):
pub use builder::{
    estimate_deny_value,
    estimate_expected_damage,
    estimate_kill_soon,
    estimate_rescue_value,
    from_sim_step,
    hypothetical,
    step_path_danger,
};
// Crate-internal API (was `pub(crate)` in old outcome.rs):
// `compute_score_core` is re-exported for `policy/tests.rs` which imports it
// via `crate::combat::ai::outcome::compute_score_core` in property tests.
// Step 4.10 removed the non-test consumer (factors/offensive.rs), so this
// re-export is test-only. Drops in 4.12 together with the function itself.
#[cfg(test)]
pub(crate) use builder::compute_score_core;

use crate::combat::ai::factors::PlanFactors;
use serde::{Deserialize, Serialize};

/// Structured estimate of a single plan step's consequences.
///
/// As of step 4.8, contains two layers:
///
/// **Fact fields (new, step 4.8)** — raw, policy-free measurements derived from
/// the sim step or the ability def. Consumers apply policy formulas from
/// `combat::ai::policy::*` to derive HP-equivalent scores.
///
/// **Legacy fields (deprecated)** — policy-baked values from wave 1 (steps
/// 4.0–4.5). Consumers still read these; migration to fact fields happens in
/// 4.10–4.11. All legacy fields drop in 4.12 together with the schema bump
/// v27 → v28.
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

    // ── LEGACY (drop in 4.12 after consumer migration to fact fields) ──

    /// HP-equivalent damage score (policy-baked). Use `enemy_damage` /
    /// `ally_damage` / `hp_restored` + policy functions instead. Drop in 4.12.
    #[deprecated(note = "Use enemy_damage / ally_damage / hp_restored + policy::*. Drop in 4.12.")]
    pub expected_damage: f32,
    /// CC / armor-debuff / vuln denial value (policy-baked). Use
    /// `cc_turns_applied`, `vulnerability_applied`, `armor_shred_applied` +
    /// `policy::cc::value(...)` instead. Drop in 4.12.
    #[deprecated(note = "Use cc_turns_applied / vulnerability_applied / armor_shred_applied + policy::cc::value. Drop in 4.12.")]
    pub deny_value: f32,
    /// Heal value with urgency baked-in. Use `hp_restored` + `policy::heal::value(...)` instead.
    /// Drop in 4.12.
    #[deprecated(note = "Use hp_restored + policy::heal::value. Drop in 4.12.")]
    pub rescue_value: f32,
    /// Dead placeholder — never filled since step 5 used TerminalScore instead.
    /// Drop in 4.12.
    #[deprecated(note = "Dead placeholder (never populated). Drop in 4.12.")]
    pub board_pressure: f32,
    /// Reserved for step 17 (geometry awareness); will be added when needed.
    /// Drop in 4.12.
    #[deprecated(note = "Reserved for step 17 geometry; add when needed. Drop in 4.12.")]
    pub geometry_gain: f32,
    /// Δdanger from Move step (worst path danger). Replaced by `path_max_danger`.
    /// Drop in 4.12.
    #[deprecated(note = "Replaced by path_max_danger. Drop in 4.12.")]
    pub exposure_delta: f32,
    /// Signed resource cost (negative = spent). Replaced by `ap_spent` /
    /// `mana_spent` / `rage_spent` / `other_resource_spent`. Drop in 4.12.
    #[deprecated(note = "Replaced by ap_spent / mana_spent / rage_spent / other_resource_spent. Drop in 4.12.")]
    pub resource_swing: f32,
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
    /// Serialized into JSONL as of schema v23 (step 5.6). Old v22 logs
    /// deserialize via `#[serde(default)]` → zero-filled `TerminalScore`,
    /// preserving backward compatibility.
    #[serde(default)]
    pub terminal: crate::combat::ai::planning::terminal::TerminalScore,
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
    /// Step 7.4: raw factor decomposition for this plan.
    /// Written by the initial scoring pass. Default PlanFactors::default().
    #[serde(default)]
    pub raw_factors: PlanFactors,
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

    #[test]
    #[allow(deprecated)]
    fn default_outcome_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.expected_damage, 0.0);
        assert_eq!(o.p_kill_now, 0.0);
        assert_eq!(o.p_kill_soon, 0.0);
        assert_eq!(o.deny_value, 0.0);
        assert_eq!(o.rescue_value, 0.0);
        assert_eq!(o.board_pressure, 0.0);
        assert_eq!(o.exposure_delta, 0.0);
        assert_eq!(o.geometry_gain, 0.0);
        assert_eq!(o.resource_swing, 0.0);
    }

    #[test]
    fn default_annotation_is_empty() {
        let a = PlanAnnotation::default();
        assert!(a.outcomes.is_empty());
    }

    // ── Legacy parity tests ───────────────────────────────────────────────

    /// Legacy `expected_damage` default is 0 (parity with pre-4.8 Default::default()).
    #[test]
    #[allow(deprecated)]
    fn legacy_expected_damage_default_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.expected_damage, 0.0);
    }

    /// Legacy `deny_value` default is 0 (parity).
    #[test]
    #[allow(deprecated)]
    fn legacy_deny_value_default_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.deny_value, 0.0);
    }

    /// Legacy `rescue_value` default is 0 (parity).
    #[test]
    #[allow(deprecated)]
    fn legacy_rescue_value_default_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.rescue_value, 0.0);
    }

    /// Legacy `resource_swing` default is 0 (parity).
    #[test]
    #[allow(deprecated)]
    fn legacy_resource_swing_default_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.resource_swing, 0.0);
    }

    /// Legacy `exposure_delta` default is 0 (parity).
    #[test]
    #[allow(deprecated)]
    fn legacy_exposure_delta_default_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.exposure_delta, 0.0);
    }
}
