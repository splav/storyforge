/// Goal-preserving plan repair — scaffolding (step 6.0) + goal extraction (6.1)
/// + repair affinity computation (6.2) + continuation outcome (6.5/6.6b).
///
/// This module classifies mismatch codes produced by `PlanSnapshot::mismatch()`
/// into semantic severity levels, enabling downstream logic (6.3+) to reason
/// about whether a stored goal is still achievable rather than treating every
/// state change as a reason to replan from scratch.
// goal.rs and lifecycle.rs have moved to memory/goal/.
// Re-export for backward-compat so existing callers (`repair::GoalKind`, etc.) continue to work.
pub use crate::combat::ai::memory::goal::{GoalKind, StoredGoalContext, extract_goal_context};

pub mod affinity;
pub use affinity::{RepairAffinity, RepairWeights, compute_repair_affinity};

use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use crate::combat::ai::world::snapshot::ActiveStatusView;
use crate::combat::ai::world::tags::{StatusTag, StatusTagCache};
use combat_engine::StatusId;
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

/// Context passed to `classify_mismatch` for the `actor_status_changed` branch.
///
/// - `status_delta` — pre-computed diff of status ids (added/removed vs stored).
///   `None` when the mismatch code is not `actor_status_changed` or when the
///   caller cannot compute a delta (safe fallback → `Relevant`).
/// - `status_tags` — cache used to look up the semantic tag of each status id.
pub struct MismatchContext<'a> {
    pub status_delta: Option<&'a StatusDelta>,
    pub status_tags: &'a StatusTagCache,
}

#[cfg(test)]
impl MismatchContext<'_> {
    /// Construct a minimal context for unit tests: no delta, empty cache.
    pub fn for_test(status_tags: &StatusTagCache) -> MismatchContext<'_> {
        MismatchContext { status_delta: None, status_tags }
    }
}

/// Classify a raw mismatch code from `PlanSnapshot::mismatch()` into a
/// semantic `ContinuationSeverity`.
///
/// Pure function — no side effects, no allocations beyond those inside
/// `classify_status_change` when a delta is provided.
///
/// The mapping is intentionally exhaustive over the 8 codes currently produced
/// by `mismatch()`. Unknown codes fall through to `Invalidating` as the safe
/// default (better to replan unnecessarily than to continue an invalid goal).
pub fn classify_mismatch(code: &'static str, ctx: &MismatchContext<'_>) -> ContinuationSeverity {
    match code {
        // Rage ticks are a natural side effect of AoO / round mechanics.
        // The goal (target, position, method) is unaffected.
        "actor_rage_changed" => ContinuationSeverity::Cosmetic,

        // Status change: severity depends on what changed (HardCC/Compulsion set →
        // Invalidating; Buff lost / SoftCC set → Relevant; pure tick → Cosmetic).
        // Falls back to Relevant when no delta is available.
        "actor_status_changed" => ctx
            .status_delta
            .map(|d| classify_status_change(d, ctx.status_tags))
            .unwrap_or(ContinuationSeverity::Relevant),

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

/// Classify the severity of an `actor_status_changed` event given the status diff.
///
/// Priority order (highest severity wins):
/// 1. HardCC or Compulsion added → `Invalidating` (actor is stunned / forced-target).
/// 2. Buff removed → `Relevant` (lost protection, goal achievability changes).
/// 3. SoftCC added → `Relevant` (soft constraint, actor can still act).
/// 4. Dot/other added → `Relevant` (minor world state change).
/// 5. Empty added/removed → `Cosmetic` (pure tick: counter changed, set unchanged).
fn classify_status_change(delta: &StatusDelta, cache: &StatusTagCache) -> ContinuationSeverity {
    // 1. HardCC or Compulsion set — actor is now controlled or forced-target.
    for added in &delta.added {
        let tags = cache.get(added);
        if tags.contains_tag(StatusTag::HardCC) || tags.contains_tag(StatusTag::Compulsion) {
            return ContinuationSeverity::Invalidating;
        }
    }
    // 2. Buff lost — actor has lost a defensive/protective status.
    for removed in &delta.removed {
        if cache.get(removed).contains_tag(StatusTag::Buff) {
            return ContinuationSeverity::Relevant;
        }
    }
    // 3. SoftCC added — actor is slowed or disoriented, but can still pursue the goal.
    for added in &delta.added {
        if cache.get(added).contains_tag(StatusTag::SoftCC) {
            return ContinuationSeverity::Relevant;
        }
    }
    // 4. Pure tick — counter decremented, status set itself is unchanged.
    if delta.added.is_empty() && delta.removed.is_empty() {
        return ContinuationSeverity::Cosmetic;
    }
    // 5. Any other set change (Dot added, unknown status) → Relevant.
    ContinuationSeverity::Relevant
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
        // LastStand is now an EvaluationMode, not a TacticalIntent; adapt-triggered
        // plans that used LastStand scoring still carry ProtectSelf intent via the
        // global intent, so only ProtectSelf is needed here.
        (GoalKind::Retreat { .. }, TacticalIntent::ProtectSelf) => true,
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

// ── Status delta (step 9.B) ───────────────────────────────────────────────────

/// Diff between the status set stored at plan-capture time and the current
/// actor status set.
///
/// Used in commit 3 by `classify_mismatch` to determine the severity of an
/// `actor_status_changed` mismatch without hard-coding severity per code.
/// Introduced here (commit 0) as a pure shared helper; no consumers yet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusDelta {
    /// Status ids present in `current` but absent in `stored`.
    pub added: Vec<StatusId>,
    /// Status ids present in `stored` but absent in `current`.
    pub removed: Vec<StatusId>,
}

/// Compute the diff between a stored status list and the current one.
///
/// Pure — no side effects, no allocations beyond the returned vecs.
/// `stored` is a snapshot of `StatusId`s captured when the plan was made;
/// `current` is the live `ActiveStatusView` slice on the actor right now.
///
/// The function compares **presence** only (not round counts / dot values),
/// which is what matters for goal-validity classification.
pub fn compute_status_delta(
    stored: &[StatusId],
    current: &[ActiveStatusView],
) -> StatusDelta {
    let added: Vec<StatusId> = current
        .iter()
        .filter(|av| !stored.contains(&av.id))
        .map(|av| av.id.clone())
        .collect();
    let removed: Vec<StatusId> = stored
        .iter()
        .filter(|sid| !current.iter().any(|av| &av.id == *sid))
        .cloned()
        .collect();
    StatusDelta { added, removed }
}

/// Variant of [`compute_status_delta`] for callers that have an engine
/// `ActiveStatus` slice (via `UnitView` Deref to engine `Unit`). Compares
/// `id` fields only — identical semantics to the `ActiveStatusView` form.
pub fn compute_status_delta_engine(
    stored: &[StatusId],
    current: &[combat_engine::state::ActiveStatus],
) -> StatusDelta {
    let added: Vec<StatusId> = current
        .iter()
        .filter(|av| !stored.contains(&av.id))
        .map(|av| av.id.clone())
        .collect();
    let removed: Vec<StatusId> = stored
        .iter()
        .filter(|sid| !current.iter().any(|av| &av.id == *sid))
        .cloned()
        .collect();
    StatusDelta { added, removed }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
