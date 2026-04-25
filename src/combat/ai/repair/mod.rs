/// Goal-preserving plan repair — scaffolding (step 6.0) + goal extraction (6.1)
/// + repair affinity computation (6.2) + continuation outcome (6.5).
///
/// This module classifies mismatch codes produced by `PlanSnapshot::mismatch()`
/// into semantic severity levels, enabling downstream logic (6.3+) to reason
/// about whether a stored goal is still achievable rather than treating every
/// state change as a reason to replan from scratch.
pub mod goal;
pub use goal::{GoalKind, StoredGoalContext, extract_goal_context};

pub mod affinity;
pub use affinity::{RepairAffinity, RepairWeights, compute_repair_affinity};

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::types::PlanStep;
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

// ── ContinuationOutcome (step 6.5) ───────────────────────────────────────────

/// Reason why a stored goal was abandoned on the current tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbandonReason {
    /// The plan-snapshot mismatch was `Invalidating` (target dead/gone, actor
    /// displaced, etc.) — the goal cannot be continued at all.
    InvalidatingMismatch,
    /// The goal's TTL has elapsed (current_round - created_round >= ttl).
    TtlExpired,
    /// The fresh plan's intent does not match the stored goal's kind/target.
    IntentDiverged,
}

/// High-level outcome of the goal-preservation check on each tick.
///
/// Produced by `classify_continuation_outcome` and written into
/// `PlanDivergenceEntry.continuation_outcome` (schema v24, step 6.5).
///
/// Default = `NoStoredGoal` via `serde(default)` for backward compat with v23
/// logs that lack the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContinuationOutcome {
    /// Stored goal preserved; fresh plan uses the same ability on the same
    /// entity as stored — method unchanged.
    GoalPreservedMethodPreserved,
    /// Stored goal preserved (intent/target match), but fresh plan uses a
    /// different ability or no Cast step at index 1.
    GoalPreservedMethodChanged,
    /// Stored goal explicitly abandoned for the given reason.
    GoalAbandoned { reason: AbandonReason },
    /// No stored goal — first tick after Cast/EndTurn, or memory cleared.
    #[default]
    NoStoredGoal,
}

impl ContinuationOutcome {
    /// Serde `default` helper: used as the backward-compat default for v23
    /// log entries that predate schema v24.
    pub fn no_stored_goal() -> Self {
        Self::NoStoredGoal
    }
}

// ── Outcome classifier (step 6.5) ─────────────────────────────────────────────

/// Classify the continuation outcome for a single AI tick.
///
/// Inputs:
/// - `stored` — the goal context from the previous tick (`None` = first tick).
/// - `fresh_intent` — intent of the fresh plan selected this tick.
/// - `fresh_step1` — `fresh_plan.steps.get(1)` (the Cast step, if any).
/// - `severity` — mismatch severity from `check_continuation`, if any.
/// - `age` — `current_round.saturating_sub(stored.created_round)`.
///
/// Pure function — no side effects.
pub fn classify_continuation_outcome(
    stored: Option<&StoredGoalContext>,
    fresh_intent: TacticalIntent,
    fresh_step1: Option<&PlanStep>,
    severity: Option<ContinuationSeverity>,
    age: u32,
) -> ContinuationOutcome {
    let Some(stored) = stored else {
        return ContinuationOutcome::NoStoredGoal;
    };
    if matches!(severity, Some(ContinuationSeverity::Invalidating)) {
        return ContinuationOutcome::GoalAbandoned {
            reason: AbandonReason::InvalidatingMismatch,
        };
    }
    if age >= stored.ttl as u32 {
        return ContinuationOutcome::GoalAbandoned {
            reason: AbandonReason::TtlExpired,
        };
    }
    if !goal_kind_matches_intent(&stored.kind, fresh_intent) {
        return ContinuationOutcome::GoalAbandoned {
            reason: AbandonReason::IntentDiverged,
        };
    }
    let method_match = match (&stored.planned_ability, fresh_step1) {
        (Some(stored_ab), Some(PlanStep::Cast { ability, .. })) => stored_ab == ability,
        _ => false,
    };
    if method_match {
        ContinuationOutcome::GoalPreservedMethodPreserved
    } else {
        ContinuationOutcome::GoalPreservedMethodChanged
    }
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

    // ── classify_continuation_outcome tests (step 6.5) ──────────────────────

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
            None,
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::NoStoredGoal);
    }

    #[test]
    fn classify_invalidating_severity_yields_abandoned() {
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(1) },
            None,
            Some(ContinuationSeverity::Invalidating),
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandoned { reason: AbandonReason::InvalidatingMismatch }
        );
    }

    #[test]
    fn classify_ttl_expired() {
        let stored = stored_finish(ent(1)); // ttl = 2
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(1) },
            None,
            None,
            2, // age == ttl → expired
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandoned { reason: AbandonReason::TtlExpired }
        );
    }

    #[test]
    fn classify_intent_diverged() {
        // stored: Finish on ent(1), fresh: FocusTarget on ent(2)
        let stored = stored_finish(ent(1));
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target: ent(2) },
            None,
            None,
            0,
        );
        assert_eq!(
            outcome,
            ContinuationOutcome::GoalAbandoned { reason: AbandonReason::IntentDiverged }
        );
    }

    #[test]
    fn classify_method_preserved() {
        let target = ent(1);
        let ab = crate::core::AbilityId("slash".into());
        let stored = stored_finish(target); // planned_ability = "slash"
        let step1 = PlanStep::Cast {
            ability: ab.clone(),
            target,
            target_pos: crate::game::hex::Hex::new(1, 0),
        };
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            Some(&step1),
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedMethodPreserved);
    }

    #[test]
    fn classify_method_changed() {
        let target = ent(1);
        let stored = stored_finish(target); // planned_ability = "slash"
        // fresh uses a different ability
        let step1 = PlanStep::Cast {
            ability: crate::core::AbilityId("fireball".into()),
            target,
            target_pos: crate::game::hex::Hex::new(1, 0),
        };
        let outcome = classify_continuation_outcome(
            Some(&stored),
            TacticalIntent::FocusTarget { target },
            Some(&step1),
            None,
            0,
        );
        assert_eq!(outcome, ContinuationOutcome::GoalPreservedMethodChanged);
    }
}
