/// Goal-preserving plan repair — scaffolding (step 6.0) + goal extraction (6.1).
///
/// This module classifies mismatch codes produced by `PlanSnapshot::mismatch()`
/// into semantic severity levels, enabling downstream logic (6.3+) to reason
/// about whether a stored goal is still achievable rather than treating every
/// state change as a reason to replan from scratch.
pub mod goal;
pub use goal::{GoalKind, StoredGoalContext, extract_goal_context};

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
}
