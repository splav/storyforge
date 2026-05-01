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
use crate::combat::ai::modifiers::ModifierContribution;
use serde::{Deserialize, Serialize};

// ── RejectReason ─────────────────────────────────────────────────────────────

/// Why a plan is ineligible under a particular agenda item — step 11.7.
///
/// Set by `ItemScoringStage` whenever `eligible = false`.
/// `None` when the plan is eligible (`eligible = true`).
///
/// Serialisable for log persistence (`reject_reasons_per_item` on `PlanAnnotation`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// ProtectSelf item: plan is not defensive (SelfSurvival ≤ epsilon).
    NotDefensive,
    /// FocusTarget item: plan does not engage the target offensively (no Cast
    /// step targets the agenda target). Step 11.8 §A: this is the primary-path
    /// failure; in `ForcedTargeting` band the ApproachTarget fallback may still
    /// salvage the plan — if even ApproachTarget fails, see `NotApproachingTarget`.
    NotOffensiveVsTarget,
    /// FocusTarget item under `ForcedTargeting` band only: pool-level fallback
    /// activated (no offensive plan in the pool) but this plan also fails the
    /// ApproachTarget guard (no Move step OR final_pos not strictly closer to
    /// taunter than start_pos). Step 11.8 §A.
    NotApproachingTarget,
    /// FocusTarget/ApplyCC item: `item.target` field is `None` (build_agenda
    /// did not assign a target); treated as eligible by ItemScoringStage.
    /// Included here for completeness / future masking.
    NoTarget,
    /// ProtectAlly item: same as `NoTarget` but for ally — separate for clarity.
    NoAllyTarget,
    /// Catch-all for future filters; current code should not emit this.
    Other,
}

// ── PerItemEval ───────────────────────────────────────────────────────────────

/// Per-agenda-item scoring cache for one plan — step 11.4.
///
/// Holds the intent-dependent factors for a specific `AgendaItem` evaluated
/// against a specific plan.  `ItemScoringStage` fills `per_item[i]` for every
/// item `i` in the agenda; `PickBestStage` reads them during composition.
///
/// Runtime-only — not serialised (no `#[serde]`).  Schema bump lives in 11.6.
#[derive(Clone, Copy, Debug)]
pub struct PerItemEval {
    /// `compute_plan_intent_sum(plan, item.intent_for_scoring(), ctx)`.
    pub intent_factor: f32,
    /// `compute_plan_tempo_gain(plan, item.intent_for_scoring(), ctx)`.
    pub tempo_factor: f32,
    /// `true` when the plan is eligible under this item's intent-specific filter.
    /// `false` means the plan is incompatible with this agenda item and must be
    /// skipped during composition.
    /// Sources: ProtectSelf → `plan_is_defensive` check;
    ///          FocusTarget → `plan_is_offensive_vs` check.
    /// Defaults to `true` (no masking for general intent kinds).
    pub eligible: bool,
    /// Why the plan is ineligible (`None` when `eligible = true`).
    /// Populated by `ItemScoringStage` alongside `eligible = false` — step 11.7.
    pub reject_reason: Option<RejectReason>,
    /// Plan-aware considerations overlay.  Populated by `OverlayConsiderationsStage`
    /// (after `RepairAffinityStage`) with accurate feasibility / leverage / safety
    /// values derived from plan data.  Falls back to item-level considerations
    /// when not yet populated (zero-default `IntentConsiderations`).
    pub considerations: crate::combat::ai::intent::considerations::IntentConsiderations,
}

impl Default for PerItemEval {
    fn default() -> Self {
        Self {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true, // eligible by default; masking stages set to false
            reject_reason: None,
            considerations: crate::combat::ai::intent::considerations::IntentConsiderations::default(),
        }
    }
}

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
    #[serde(default, with = "crate::combat::ai::log::serde_helpers::f32_finite")]
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
    /// Step 8.B: per-modifier additive contributions applied in PlanModifiersStage.
    /// Empty until that stage runs; populated in canonical PLAN_MODIFIERS order
    /// [summon_bonus, trade_bonus, repair_bonus]. Sum equals the score delta
    /// produced by the stage. Pure observability — does not influence picking.
    #[serde(default)]
    pub modifiers: Vec<ModifierContribution>,
    /// Step 9.A: per-Cast-step effective AI tags (cache lookup with override applied).
    /// Length equals the number of Cast steps in the plan; Move steps contribute nothing.
    /// Diagnostic only — no consumer reads this in 9.A. Consumers come in 9.B.
    /// Schema-additive via `#[serde(default)]`; v29 logs without this field
    /// deserialise as an empty vec.
    #[serde(default)]
    pub effective_ai_tags: Vec<crate::combat::ai::world::tags::AbilityTagSet>,
    /// Step 10.0: critic hits applied to this plan (critic + multiplier + reason triples).
    /// Empty until CriticsStage runs or when no critics fired.
    /// Schema-additive via `#[serde(default)]`; v30 logs without this field
    /// deserialise as an empty vec.
    #[serde(default)]
    pub critics: Vec<crate::combat::ai::critics::CriticHit>,

    // ── Step 11.4/11.6 fields ─────────────────────────────────────────────────

    /// Score immediately after the initial `score_plans_with_raw` pass,
    /// before any pipeline stages run.  Used in `PickBestStage` as the base
    /// for additive per-item composition: `composed = score_initial +
    /// intent_delta + tempo_delta + W × cdot`.
    /// Not serialised — runtime-only field.
    #[serde(skip)]
    pub score_initial: f32,

    /// Per-agenda-item scoring cache.  `per_item[i]` holds the intent-dependent
    /// factors for agenda item `i`.  Populated by `ItemScoringStage`; consumed
    /// by `PickBestStage` during composition.
    /// Not serialised — runtime-only field.
    #[serde(skip)]
    pub per_item: Vec<PerItemEval>,

    /// Winning agenda-item index (into `Agenda::items`) as chosen by
    /// `PickBestStage`.  `None` when agenda is empty (legacy path) or before
    /// `PickBestStage` runs.
    /// Step 11.6: serialised in schema v32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda_item: Option<u8>,

    /// Step 11.6: per-agenda-item considerations overlay as applied during
    /// `PickBestStage` composition. `considerations_per_item[i]` is the
    /// `IntentConsiderations` from `per_item[i]` (plan-aware composite overlay).
    /// Empty when agenda is absent (legacy path) or before `PickBestStage` runs.
    /// Serialised in schema v32. Factors (`intent_factor`, `tempo_factor`) are
    /// runtime-only and live only in `per_item` (not serialised).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub considerations_per_item: Vec<crate::combat::ai::intent::considerations::IntentConsiderations>,

    /// Step 11.7: per-agenda-item reject reasons as set by `ItemScoringStage`.
    /// `reject_reasons_per_item[i]` is `Some(reason)` when item `i` was
    /// rejected (eligible=false), else `None`.
    /// Empty when agenda is absent or before `PickBestStage` snapshots them.
    /// Schema-additive: v32 logs without this field deserialise as empty vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_reasons_per_item: Vec<Option<RejectReason>>,

    // ── P3a fields ────────────────────────────────────────────────────────────

    /// P3a: typed log of score-affecting effects accumulated during pipeline.
    /// Currently P3a.0: structurally present, not yet populated by stages.
    /// Migration progress: P3a.{1..5} — each migrates one stage class to push
    /// hits here.
    /// Not serialised in P3a (no schema bump); P3b adds JSONL exposure.
    #[serde(skip)]
    pub score_trace: crate::combat::ai::pipeline::score_trace::ScoreTrace,
}

/// Adaptation reason + original (pre-adaptation) score for a single plan.
/// Written by `ModeSelectionStage`; consumed by `FinalizeStage` to build
/// `IntentReason::Adapted` for the winning plan.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdaptationData {
    pub reason: crate::combat::ai::adapt::AdaptationReason,
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
    /// Deterministic jitter added to this plan's score before argmax.
    /// Written in 8.C commit 2 when `apply_pick_jitter` is wired in.
    /// Zero for pre-8.C logs (forward-compat via `#[serde(default)]`).
    #[serde(default)]
    pub noise_applied: f32,
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

    /// Default-constructed PickInfo has noise_applied = 0.0.
    #[test]
    fn pick_info_default_noise_applied_zero() {
        let pi = PickInfo::default();
        assert_eq!(pi.noise_applied, 0.0);
    }

    /// JSON without `noise_applied` (pre-8.C logs) deserialises with 0.0.
    #[test]
    fn pick_info_v29_load_pre_8c_round_trip() {
        // Round-trip via serialise → strip noise_applied → deserialise.
        // This simulates a pre-8.C log entry that was written before the field existed.
        let original = PickInfo::default();
        let mut v: serde_json::Value = serde_json::to_value(&original).expect("serialize ok");
        // Remove the field to simulate a pre-8.C log.
        v.as_object_mut().unwrap().remove("noise_applied");
        let json_without_field = serde_json::to_string(&v).expect("re-serialize ok");

        let pi: PickInfo = serde_json::from_str(&json_without_field)
            .expect("pre-8.C PickInfo should deserialise OK");
        assert_eq!(pi.noise_applied, 0.0, "missing field should default to 0.0");
    }

    // ── Step 9.A: PlanAnnotation.effective_ai_tags tests ─────────────────────

    /// Default PlanAnnotation has empty effective_ai_tags.
    #[test]
    fn plan_annotation_default_effective_ai_tags_empty() {
        let a = PlanAnnotation::default();
        assert!(
            a.effective_ai_tags.is_empty(),
            "default effective_ai_tags must be empty vec"
        );
    }

    /// A v29 JSON log entry without `effective_ai_tags` field deserialises
    /// as empty vec (forward compatibility via #[serde(default)]).
    #[test]
    fn plan_annotation_serde_v29_log_without_effective_ai_tags_deserialises() {
        let original = PlanAnnotation::default();
        let mut v: serde_json::Value = serde_json::to_value(&original).expect("serialize ok");
        v.as_object_mut().unwrap().remove("effective_ai_tags");
        let json_without_field = serde_json::to_string(&v).expect("re-serialize ok");

        let ann: PlanAnnotation = serde_json::from_str(&json_without_field)
            .expect("v29 PlanAnnotation without effective_ai_tags must deserialise OK");
        assert!(
            ann.effective_ai_tags.is_empty(),
            "missing effective_ai_tags field must deserialise as empty vec"
        );
    }

    /// PlanAnnotation with effective_ai_tags survives a serde round-trip.
    #[test]
    fn plan_annotation_serde_round_trip_with_effective_ai_tags() {
        use crate::combat::ai::world::tags::{AbilityTag, AbilityTagSet};

        let ann = PlanAnnotation {
            effective_ai_tags: vec![AbilityTagSet::OFFENSIVE, AbilityTagSet::RESCUE],
            ..Default::default()
        };
        let json = serde_json::to_string(&ann).expect("serialize ok");
        let decoded: PlanAnnotation = serde_json::from_str(&json).expect("deserialize ok");

        assert_eq!(decoded.effective_ai_tags.len(), 2);
        assert!(decoded.effective_ai_tags[0].contains_tag(AbilityTag::Offensive));
        assert!(decoded.effective_ai_tags[1].contains_tag(AbilityTag::Rescue));
        assert!(!decoded.effective_ai_tags[0].contains_tag(AbilityTag::Rescue));
    }
}
