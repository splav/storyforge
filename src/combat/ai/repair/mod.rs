/// Goal-preserving plan repair — scaffolding (step 6.0) + goal extraction (6.1)
/// + repair affinity computation (6.2) + continuation outcome (6.5/6.6b).
///
/// This module classifies mismatch codes produced by `PlanSnapshot::mismatch()`
/// into semantic severity levels, enabling downstream logic (6.3+) to reason
/// about whether a stored goal is still achievable rather than treating every
/// state change as a reason to replan from scratch.
pub mod goal;
pub use goal::{GoalKind, StoredGoalContext, extract_goal_context};

pub mod affinity;
pub use affinity::{RepairAffinity, RepairWeights, compute_repair_affinity};

pub mod lifecycle;

use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use serde::{Deserialize, Serialize};

/// Semantic severity of a detected state mismatch between a stored plan
/// snapshot and the current world.
///
/// - `Cosmetic` — change does not affect goal achievability (rage tick,
///   mana spent on this same target).
/// - `Relevant` — goal remains achievable but the optimal method may
///   change (target moved, hp dropped — repair affinity is weakened but goal
///   is alive).
/// - `Invalidating` — goal is no longer achievable (target dead/gone, actor
///   displaced by external force, unknown state change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContinuationSeverity {
    Cosmetic,
    Relevant,
    Invalidating,
}

/// Structured result of a plan-snapshot continuation check. Replaces the raw
/// `Option<&'static str>` from `PlanSnapshot::mismatch()` in downstream layers
/// while preserving the original reason code for telemetry.
#[derive(Debug, Clone)]
pub struct PlanContinuationCheck {
    pub severity: ContinuationSeverity,
    /// Original reason code from `PlanSnapshot::mismatch()`, preserved for
    /// telemetry and log compatibility.
    pub reason_code: &'static str,
}

/// Classify a raw mismatch code from `PlanSnapshot::mismatch()` into a
/// semantic `ContinuationSeverity`.
///
/// Pure function — no side effects, no allocations.
///
/// The mapping is intentionally exhaustive over the 8 codes currently produced
/// by `mismatch()`. Unknown codes fall through to `Invalidating` as the safe
/// default (better to replan unnecessarily than to continue an invalid goal).
pub fn classify_mismatch(code: &'static str) -> ContinuationSeverity {
    match code {
        // Rage ticks are a natural side effect of AoO / round mechanics.
        // The goal (target, position, method) is unaffected.
        "actor_rage_changed" => ContinuationSeverity::Cosmetic,

        // Status change may include a CC application — could become Invalidating
        // after step 9 adds semantic tags to distinguish CC-set vs duration-tick.
        "actor_status_changed" => ContinuationSeverity::Relevant,

        // Actor took damage: self-preserve needs re-eval, but the goal is alive.
        "actor_hp_drop" => ContinuationSeverity::Relevant,

        // Actor was moved to a position that wasn't planned — goal topology broken.
        "actor_pos_mismatch" => ContinuationSeverity::Invalidating,

        // Target no longer exists in the snapshot.
        "target_gone" => ContinuationSeverity::Invalidating,

        // The entity at the target slot is a different unit entirely.
        "target_entity_changed" => ContinuationSeverity::Invalidating,

        // Target took damage (e.g. from another actor) — goal may complete sooner.
        "target_hp_drop" => ContinuationSeverity::Relevant,

        // Target moved: method may change, goal (destroy/pressure/CC that unit) lives.
        "target_moved" => ContinuationSeverity::Relevant,

        // Unknown code → safe default: replan rather than risk continuing an
        // invalid goal. Callers should not rely on unknown codes being Invalidating
        // in perpetuity; the match arm is here to provide an exhaustiveness guarantee.
        _ => ContinuationSeverity::Invalidating,
    }
}

// ── ContinuationOutcome (step 6.5, refined in 6.6b) ──────────────────────────

/// High-level outcome of the goal-preservation check on each tick.
///
/// Produced by `classify_continuation_outcome` and written into
/// `PlanDivergenceEntry.continuation_outcome` (schema v26, step 6.6b).
///
/// ## Backward-compat aliases (v25 logs)
/// - `goal_preserved_method_preserved` → `GoalPreservedMethodDelivered`
/// - `goal_preserved_method_changed`   → `GoalPreservedInTransit`
/// - old `goal_abandoned { reason }` (v25 write-time shape) → `LegacyV25Abandoned`
///   explicit bucket; voluntary/reactive split was not recorded at write-time.
///
/// Default = `NoStoredGoal` via `#[serde(default)]` for backward compat with
/// v23/v24 logs that lack the field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContinuationOutcome {
    /// First tick or after Cast/EndTurn — no stored goal to compare.
    #[default]
    #[serde(alias = "no_stored_goal")]
    NoStoredGoal,

    /// Fresh decision is Cast or MoveAndCast on same target/ally as stored goal —
    /// actor reached the climax of the planned arc this tick.
    #[serde(alias = "goal_preserved_method_preserved")]
    GoalPreservedMethodDelivered,

    /// Fresh decision is Move-only (no Cast) but fresh intent matches stored
    /// goal — actor is still committed and walking toward it.
    #[serde(alias = "goal_preserved_method_changed")]
    GoalPreservedInTransit,

    /// Goal abandoned because environment/system forced it (taunt, panic,
    /// adaptation, finisher opportunity, viability fallback).
    /// `source` is the `IntentReason::code()` of the fresh decision —
    /// kept as `String` for forward compat with new selection_kind codes.
    GoalAbandonedReactive { source: String },

    /// Goal abandoned because actor freely picked another target/intent
    /// (selection_kind ∈ best_priority / reposition / setup_aoe / protect_ally /
    ///  apply_cc / no_rule_default / urgency / adapted-with-non-reactive-prior).
    /// This is the real "weak commitment" signal — the metric to minimize.
    GoalAbandonedVoluntary,

    /// Severity = Invalidating (target dead, actor pos mismatch).
    GoalAbandonedInvalidating,

    /// Stored goal age >= ttl.
    GoalAbandonedTtlExpired,

    /// Legacy v25 log entry: old `goal_abandoned { reason }` shape written before
    /// the voluntary/reactive split was introduced in schema v26.
    /// Preserved as an explicit bucket so v25 entries do not silently inflate
    /// `NoStoredGoal` and distort post-v26 analysis.
    #[serde(rename = "goal_abandoned")]
    LegacyV25Abandoned { reason: String },
}

impl ContinuationOutcome {
    /// Serde `default` helper: used as the backward-compat default for v23/v24
    /// log entries that predate schema v24/v25.
    pub fn no_stored_goal() -> Self {
        Self::NoStoredGoal
    }
}

// ── FreshDecisionKind (step 6.6b) ─────────────────────────────────────────────

/// Whether the fresh AI decision involves an ability cast this tick,
/// a move-only step, or passing.
///
/// Used in `classify_continuation_outcome` to distinguish
/// `GoalPreservedMethodDelivered` (Cast) from `GoalPreservedInTransit` (Move).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshDecisionKind {
    /// Fresh = CastInPlace or MoveAndCast — actual ability use this tick.
    Cast,
    /// Fresh = Move only — actor walking, no ability cast.
    Move,
    /// Fresh = EndTurn — pass.
    EndTurn,
}

// ── Outcome classifier (step 6.5, refined in 6.6b) ────────────────────────────

/// Classify the continuation outcome for a single AI tick.
///
/// Inputs:
/// - `stored` — the goal context from the previous tick (`None` = first tick).
/// - `fresh_intent` — intent of the fresh plan selected this tick.
/// - `fresh_decision_kind` — whether the fresh decision casts, moves, or passes.
/// - `fresh_reason` — `IntentReason` of the fresh chosen plan; used to
///   discriminate reactive vs voluntary abandons.
/// - `severity` — mismatch severity from `check_continuation`, if any.
/// - `age` — `current_round.saturating_sub(stored.created_round)`.
///
/// Pure function — no side effects.
pub fn classify_continuation_outcome(
    stored: Option<&StoredGoalContext>,
    fresh_intent: TacticalIntent,
    fresh_decision_kind: FreshDecisionKind,
    fresh_reason: &IntentReason,
    severity: Option<ContinuationSeverity>,
    age: u32,
) -> ContinuationOutcome {
    let Some(stored) = stored else {
        return ContinuationOutcome::NoStoredGoal;
    };

    // Order: hard invalidation first, then preservation, then abandon classification.
    if matches!(severity, Some(ContinuationSeverity::Invalidating)) {
        return ContinuationOutcome::GoalAbandonedInvalidating;
    }
    if age >= stored.ttl as u32 {
        return ContinuationOutcome::GoalAbandonedTtlExpired;
    }

    if goal_kind_matches_intent(&stored.kind, fresh_intent) {
        return match fresh_decision_kind {
            FreshDecisionKind::Cast => ContinuationOutcome::GoalPreservedMethodDelivered,
            FreshDecisionKind::Move | FreshDecisionKind::EndTurn => {
                ContinuationOutcome::GoalPreservedInTransit
            }
        };
    }

    // Goal abandoned — distinguish reactive vs voluntary by fresh_reason.code().
    if is_reactive_reason(fresh_reason) {
        ContinuationOutcome::GoalAbandonedReactive {
            source: fresh_reason.code().to_owned(),
        }
    } else {
        ContinuationOutcome::GoalAbandonedVoluntary
    }
}

/// Returns `true` when the intent reason is an environmental/system override —
/// i.e. the actor did not freely abandon their goal, but was forced by an
/// external constraint (taunt, panic, viability fallback, finisher opportunity,
/// ally rescue, urgent threat).
fn is_reactive_reason(reason: &IntentReason) -> bool {
    matches!(
        reason.code(),
        "taunt_forced"
            | "taunt_cc"
            | "panic_override"
            | "viability_fallback"
            | "midpanic_fallback"
            | "protect_self_no_defensive"
            | "expected_self_lethal"
            | "killable"
            // Step 6.8.B: ally needing rescue is contractual abandon, not free choice.
            | "protect_ally"
            // Step 6.8.B: urgency fires when self_preserve × danger crosses threshold —
            // a forced reaction to immediate threat, not a goal-driven decision.
            | "urgency"
    )
}

/// Returns `true` when `kind` and `intent` describe the same goal on the
/// same entity or region.
///
/// Uses explicit match instead of `matches!` macro because guards referencing
/// pattern bindings on both sides cannot be expressed in `matches!`.
fn goal_kind_matches_intent(kind: &GoalKind, intent: TacticalIntent) -> bool {
    match (kind, intent) {
        (GoalKind::Finish { target: a }, TacticalIntent::FocusTarget { target: b }) => *a == b,
        (GoalKind::Pressure { target: a }, TacticalIntent::FocusTarget { target: b }) => *a == b,
        (GoalKind::DisableEnemy { target: a }, TacticalIntent::ApplyCC { target: b }) => *a == b,
        (GoalKind::HealAlly { ally: a }, TacticalIntent::ProtectAlly { ally: b }) => *a == b,
        (GoalKind::Retreat { .. }, TacticalIntent::ProtectSelf | TacticalIntent::LastStand) => true,
        (GoalKind::SetupAOE { .. }, TacticalIntent::SetupAOE) => true,
        (GoalKind::Reposition { .. }, TacticalIntent::Reposition) => true,
        _ => false,
    }
}

/// Returns `true` when `outcome` indicates the stored goal should be cleared
/// from `AiMemory.last_goal`. Used by `goal_lifecycle::post_tick` and tests.
pub fn is_abandoned_outcome(outcome: &ContinuationOutcome) -> bool {
    matches!(
        outcome,
        ContinuationOutcome::GoalAbandonedTtlExpired
            | ContinuationOutcome::GoalAbandonedInvalidating
            | ContinuationOutcome::GoalAbandonedVoluntary
            | ContinuationOutcome::GoalAbandonedReactive { .. }
            | ContinuationOutcome::LegacyV25Abandoned { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mismatch code produced by `PlanSnapshot::mismatch()` must map to
    /// an explicit (non-wildcard) severity.  This test locks in the full table
    /// and guards against silent fall-through when new codes are added.
    #[test]
    fn classify_all_existing_codes_have_explicit_severity() {
        let table: &[(&'static str, ContinuationSeverity)] = &[
            ("actor_rage_changed", ContinuationSeverity::Cosmetic),
            ("actor_status_changed", ContinuationSeverity::Relevant),
            ("actor_hp_drop", ContinuationSeverity::Relevant),
            ("actor_pos_mismatch", ContinuationSeverity::Invalidating),
            ("target_gone", ContinuationSeverity::Invalidating),
            ("target_entity_changed", ContinuationSeverity::Invalidating),
            ("target_hp_drop", ContinuationSeverity::Relevant),
            ("target_moved", ContinuationSeverity::Relevant),
        ];
        for (code, expected) in table {
            assert_eq!(
                classify_mismatch(code),
                *expected,
                "classify_mismatch({code:?}) returned wrong severity"
            );
        }
    }

    #[test]
    fn cosmetic_codes_dont_invalidate_goal() {
        assert_eq!(
            classify_mismatch("actor_rage_changed"),
            ContinuationSeverity::Cosmetic,
        );
    }

    #[test]
    fn invalidating_codes_safe_default() {
        // Unknown codes must always produce Invalidating to avoid continuing
        // a goal that may be invalid under an unanticipated world change.
        assert_eq!(
            classify_mismatch("garbage_xyz"),
            ContinuationSeverity::Invalidating,
        );
    }

    // ── classify_continuation_outcome tests (step 6.5, updated in 6.6b) ────────

    fn ent(bits: u64) -> bevy::prelude::Entity {
        bevy::prelude::Entity::from_bits(bits)
    }

    fn stored_finish(target: bevy::prelude::Entity) -> StoredGoalContext {
        StoredGoalContext {
            kind: GoalKind::Finish { target },
            region_anchor: crate::game::hex::Hex::new(0, 0),
            region_radius: 2,
            planned_ability: Some(crate::core::AbilityId("slash".into())),
            ttl: 2,
            confidence: 1.0,
            created_round: 1,
            // Severity-check fields zeroed — outcome tests don't exercise check_continuation.
            expected_actor_pos: crate::game::hex::Hex::new(0, 0),
            actor_hp_at_store: 0,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            target_hp_at_store: 0,
            target_pos_at_store: crate::game::hex::Hex::new(0, 0),
        }
    }

    #[test]
    fn classify_no_stored_goal_when_stored_none() {
        let outcome = classify_continuation_outcome(
            None,
            TacticalIntent::FocusTarget { target: ent(1) },
            FreshDecisionKind::Move,
            &IntentReason::NoRuleDefault,
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::NoStoredGoal);
    }

    #[test]
    fn classify_invalidating_severity_yields_abandoned_invalidating() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(1) },
            FreshDecisionKind::Move,
            &IntentReason::NoRuleDefault,
            Some(ContinuationSeverity::Invalidating),
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalAbandonedInvalidating);
    }

    #[test]
    fn classify_ttl_expired() {
        let stored = stored_finish(ent(1)); // ttl = 2
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(1) },
            FreshDecisionKind::Move,
            &IntentReason::NoRuleDefault,
            None,
            2, // age == ttl → expired
        );
        assert_eq!(outcome, ContinuationOutcome::GoalAbandonedTtlExpired);
    }

    #[test]
    fn classify_voluntary_abandon_on_intent_diverged() {
        // stored: Finish on ent(1), fresh: FocusTarget on ent(2), non-reactive reason
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(2) },
            FreshDecisionKind::Move,
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalAbandonedVoluntary);
    }

    #[test]
    fn classify_reactive_abandon_on_taunt() {
        // stored: Finish on ent(1), fresh: different intent, reason = TauntForced
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(2) },
            FreshDecisionKind::Move,
            &IntentReason::TauntForced,
            None,
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandonedReactive { source: "taunt_forced".to_owned() }
        );
    }

    /// Step 6.8.B: protect_ally is a contractual rescue, not free choice.
    #[test]
    fn classify_reactive_abandon_on_protect_ally() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::ProtectAlly { ally: ent(3) },
            FreshDecisionKind::Move,
            &IntentReason::ProtectAlly { ally_hp_pct: 0.2, threshold: 0.4, heal_identity: 1.0 },
            None,
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandonedReactive { source: "protect_ally".to_owned() }
        );
    }

    /// Step 6.8.B: urgency fires under high self_preserve × danger — reactive.
    #[test]
    fn classify_reactive_abandon_on_urgency() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::ProtectSelf,
            FreshDecisionKind::Move,
            &IntentReason::Urgency { self_preserve: 0.8, danger: 0.7 },
            None,
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandonedReactive { source: "urgency".to_owned() }
        );
    }

    #[test]
    fn classify_reactive_abandon_on_killable() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(2) },
            FreshDecisionKind::Cast,
            &IntentReason::Killable {
                threat: 0.9,
                eff_hp: 10,
                reach_budget: 1,
                finish_target: 0.8,
            },
            None,
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandonedReactive { source: "killable".to_owned() }
        );
    }

    #[test]
    fn classify_goal_preserved_cast_yields_method_delivered() {
        let target = ent(1);
        let stored = stored_finish(target);
        // Cast decision on matching target → GoalPreservedMethodDelivered
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::Cast,
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedMethodDelivered);
    }

    #[test]
    fn classify_goal_preserved_move_yields_in_transit() {
        let target = ent(1);
        let stored = stored_finish(target);
        // Move-only decision, matching intent → GoalPreservedInTransit
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::Move,
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedInTransit);
    }

    // ── 6.7: goal_obsolete predicate (what triggers the clear in decision-block) ──

    /// Helper mirroring the `goal_obsolete` predicate in `run_ai_turn`.
    fn is_goal_obsolete(outcome: &ContinuationOutcome) -> bool {
        matches!(
            outcome,
            ContinuationOutcome::GoalAbandonedTtlExpired
                | ContinuationOutcome::GoalAbandonedInvalidating
                | ContinuationOutcome::GoalAbandonedVoluntary
                | ContinuationOutcome::GoalAbandonedReactive { .. }
                | ContinuationOutcome::LegacyV25Abandoned { .. }
        )
    }

    /// EndTurn with a matching in-transit goal → outcome is InTransit → NOT obsolete
    /// → goal should be preserved across rounds.
    #[test]
    fn last_goal_preserved_across_endturn() {
        let target = ent(1);
        let stored = stored_finish(target);
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::EndTurn,
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0, // age < ttl
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedInTransit);
        assert!(!is_goal_obsolete(&outcome), "in-transit goal must not be cleared on EndTurn");
    }

    /// CastInPlace is handled unconditionally in the decision-block (climax),
    /// but the corresponding outcome is MethodDelivered which is also not obsolete.
    #[test]
    fn last_goal_cleared_after_cast_in_place() {
        let target = ent(1);
        let stored = stored_finish(target);
        // Cast = MethodDelivered — goal reached, not an abandon.
        // Decision-block clears unconditionally for Cast; this confirms outcome is correct.
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::Cast,
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedMethodDelivered,
            "cast delivering the goal should produce MethodDelivered, not an abandon");
        // The decision-block clears regardless of obsolete flag for Cast — see run_ai_turn.
        // goal_obsolete is NOT the clearing mechanism for Cast/MoveAndCast.
    }

    /// MoveAndCast also delivers the goal; same classification as CastInPlace.
    #[test]
    fn last_goal_cleared_after_move_and_cast() {
        let target = ent(1);
        let stored = stored_finish(target);
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::Cast, // MoveAndCast → FreshDecisionKind::Cast
            &IntentReason::BestPriority { priority: 1.0 },
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedMethodDelivered);
        assert!(!is_goal_obsolete(&outcome));
    }

    /// When age >= ttl the outcome is TtlExpired → goal_obsolete = true.
    /// Covers both the decision-block path (EndTurn) and the early-return path.
    #[test]
    fn stale_goal_cleared_when_ttl_expired() {
        let target = ent(1);
        let stored = stored_finish(target); // ttl = 2
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            FreshDecisionKind::EndTurn,
            &IntentReason::NoRuleDefault,
            None,
            2, // age == ttl → expired
        );
        assert_eq!(outcome, ContinuationOutcome::GoalAbandonedTtlExpired);
        assert!(is_goal_obsolete(&outcome), "ttl-expired goal must be cleared");
    }

    /// Invalidating severity → GoalAbandonedInvalidating → goal_obsolete = true.
    #[test]
    fn stale_goal_cleared_when_invalidating() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(1) },
            FreshDecisionKind::Move,
            &IntentReason::NoRuleDefault,
            Some(ContinuationSeverity::Invalidating),
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalAbandonedInvalidating);
        assert!(is_goal_obsolete(&outcome), "invalidating goal must be cleared");
    }
}
