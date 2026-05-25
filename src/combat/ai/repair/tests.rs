//! Tests for `mod.rs` (repair) — split from the source file via `#[path]` in
//! `mod.rs` (see end of that file). Production code stays in `mod.rs`; this
//! file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../docs/testing.md): `repair/mod.rs`
//! grew to 921 LOC with tests dominating the lower half.
//!
//! `super::*` here resolves to `repair/mod.rs` (since this file is included
//! as `mod tests` inside mod.rs).

use super::*;

/// Every mismatch code produced by `PlanSnapshot::mismatch()` must map to
/// an explicit (non-wildcard) severity.
///
/// For `actor_status_changed` we assert the two representative cases:
/// - No delta (unknown caller context) → fallback `Relevant`.
/// - Delta with HardCC added → `Invalidating`.
#[test]
fn classify_all_existing_codes_have_explicit_severity() {
    let cache = StatusTagCache::default();

    // Non-status codes: check with empty context (delta=None).
    let non_status_table: &[(&'static str, ContinuationSeverity)] = &[
        ("actor_rage_changed", ContinuationSeverity::Cosmetic),
        ("actor_hp_drop", ContinuationSeverity::Relevant),
        ("actor_pos_mismatch", ContinuationSeverity::Invalidating),
        ("target_gone", ContinuationSeverity::Invalidating),
        ("target_entity_changed", ContinuationSeverity::Invalidating),
        ("target_hp_drop", ContinuationSeverity::Relevant),
        ("target_moved", ContinuationSeverity::Relevant),
    ];
    let no_delta_ctx = MismatchContext::for_test(&cache);
    for (code, expected) in non_status_table {
        assert_eq!(
            classify_mismatch(code, &no_delta_ctx),
            *expected,
            "classify_mismatch({code:?}) returned wrong severity"
        );
    }

    // actor_status_changed without delta → Relevant fallback.
    assert_eq!(
        classify_mismatch("actor_status_changed", &MismatchContext::for_test(&cache)),
        ContinuationSeverity::Relevant,
        "actor_status_changed without delta must fall back to Relevant"
    );

    // actor_status_changed with HardCC delta → Invalidating.
    use crate::combat::ai::world::tags::StatusTagSet;
    let delta = StatusDelta {
        added: vec![StatusId::from("stunned")],
        removed: vec![],
    };
    // Build a cache that maps "stunned" → HardCC.
    let mut hardcc_cache = StatusTagCache::default();
    hardcc_cache.map.insert(
        StatusId::from("stunned"),
        StatusTagSet::from_iter_tags([StatusTag::HardCC]),
    );
    let hardcc_ctx = MismatchContext { status_delta: Some(&delta), status_tags: &hardcc_cache };
    assert_eq!(
        classify_mismatch("actor_status_changed", &hardcc_ctx),
        ContinuationSeverity::Invalidating,
        "actor_status_changed with HardCC delta must be Invalidating"
    );

    // actor_status_changed with Compulsion delta → Invalidating.
    let compulsion_delta = StatusDelta {
        added: vec![StatusId::from("taunted")],
        removed: vec![],
    };
    let mut compulsion_cache = StatusTagCache::default();
    compulsion_cache.map.insert(
        StatusId::from("taunted"),
        StatusTagSet::from_iter_tags([StatusTag::Compulsion]),
    );
    let compulsion_ctx = MismatchContext {
        status_delta: Some(&compulsion_delta),
        status_tags: &compulsion_cache,
    };
    assert_eq!(
        classify_mismatch("actor_status_changed", &compulsion_ctx),
        ContinuationSeverity::Invalidating,
        "actor_status_changed with Compulsion delta must be Invalidating"
    );
}

#[test]
fn cosmetic_codes_dont_invalidate_goal() {
    let cache = StatusTagCache::default();
    assert_eq!(
        classify_mismatch("actor_rage_changed", &MismatchContext::for_test(&cache)),
        ContinuationSeverity::Cosmetic,
    );
}

#[test]
fn invalidating_codes_safe_default() {
    // Unknown codes must always produce Invalidating to avoid continuing
    // a goal that may be invalid under an unanticipated world change.
    let cache = StatusTagCache::default();
    assert_eq!(
        classify_mismatch("garbage_xyz", &MismatchContext::for_test(&cache)),
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
        planned_ability: Some(combat_engine::AbilityId("slash".into())),
        ttl: 2,
        confidence: 1.0,
        created_round: 1,
        // Severity-check fields zeroed — outcome tests don't exercise check_continuation.
        expected_actor_pos: crate::game::hex::Hex::new(0, 0),
        actor_hp_at_store: 0,
        actor_rage_at_store: 0,
        actor_status_hash: 0,
        actor_statuses_at_store: vec![],
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

// ── StatusDelta tests (step 9.B commit 0) ────────────────────────────────

fn sid(s: &str) -> StatusId { StatusId::from(s) }

fn active_status(id: &str) -> ActiveStatusView {
    ActiveStatusView { id: sid(id), rounds_remaining: 1, dot_per_tick: 0 }
}

/// New status present in `current` but not in `stored` → shows up in `added`.
#[test]
fn compute_status_delta_added_diff() {
    let stored = vec![sid("burning")];
    let current = vec![active_status("burning"), active_status("stunned")];
    let delta = compute_status_delta(&stored, &current);
    assert_eq!(delta.added, vec![sid("stunned")]);
    assert!(delta.removed.is_empty());
}

/// Status present in `stored` but gone from `current` → shows up in `removed`.
#[test]
fn compute_status_delta_removed_diff() {
    let stored = vec![sid("burning"), sid("poisoned")];
    let current = vec![active_status("burning")];
    let delta = compute_status_delta(&stored, &current);
    assert!(delta.added.is_empty());
    assert_eq!(delta.removed, vec![sid("poisoned")]);
}

/// Same status set (only counter may differ) → both `added` and `removed` empty.
#[test]
fn compute_status_delta_pure_tick_empty() {
    let stored = vec![sid("burning"), sid("poisoned")];
    let current = vec![active_status("burning"), active_status("poisoned")];
    let delta = compute_status_delta(&stored, &current);
    assert!(delta.added.is_empty(), "no new statuses");
    assert!(delta.removed.is_empty(), "no removed statuses");
}

// ── classify_status_change tests (step 9.B commit 3) ────────────────────

use crate::combat::ai::world::tags::StatusTagSet;

/// Build a StatusTagCache with a single entry.
fn cache_single(id: &str, tag: StatusTag) -> StatusTagCache {
    let mut c = StatusTagCache::default();
    c.map.insert(StatusId::from(id), StatusTagSet::from_iter_tags([tag]));
    c
}

/// HardCC added → Invalidating (actor is stunned).
#[test]
fn classify_status_change_hardcc_set_invalidates() {
    let cache = cache_single("stunned", StatusTag::HardCC);
    let delta = StatusDelta { added: vec![sid("stunned")], removed: vec![] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Invalidating);
}

/// Compulsion added → Invalidating (actor is force-targeted / taunted).
#[test]
fn classify_status_change_compulsion_set_invalidates() {
    let cache = cache_single("taunted", StatusTag::Compulsion);
    let delta = StatusDelta { added: vec![sid("taunted")], removed: vec![] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Invalidating);
}

/// SoftCC added → Relevant (actor slowed/disoriented, goal still alive).
#[test]
fn classify_status_change_softcc_set_relevant() {
    let cache = cache_single("disoriented", StatusTag::SoftCC);
    let delta = StatusDelta { added: vec![sid("disoriented")], removed: vec![] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Relevant);
}

/// Buff removed → Relevant (actor lost protection, goal achievability changes).
#[test]
fn classify_status_change_buff_removed_relevant() {
    let cache = cache_single("defending", StatusTag::Buff);
    let delta = StatusDelta { added: vec![], removed: vec![sid("defending")] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Relevant);
}

/// Dot added (tagged as Dot, not HardCC/SoftCC/Buff) → Relevant.
#[test]
fn classify_status_change_dot_added_relevant() {
    let cache = cache_single("poisoned", StatusTag::Dot);
    let delta = StatusDelta { added: vec![sid("poisoned")], removed: vec![] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Relevant);
}

/// Pure tick: no added/removed statuses → Cosmetic.
#[test]
fn classify_status_change_pure_tick_cosmetic() {
    let cache = StatusTagCache::default();
    let delta = StatusDelta { added: vec![], removed: vec![] };
    assert_eq!(classify_status_change(&delta, &cache), ContinuationSeverity::Cosmetic);
}

/// Legacy codes other than actor_status_changed are unchanged by the new context.
#[test]
fn classify_mismatch_legacy_codes_unchanged() {
    let cache = StatusTagCache::default();
    let ctx = MismatchContext::for_test(&cache);
    let table: &[(&'static str, ContinuationSeverity)] = &[
        ("actor_rage_changed",    ContinuationSeverity::Cosmetic),
        ("actor_hp_drop",         ContinuationSeverity::Relevant),
        ("actor_pos_mismatch",    ContinuationSeverity::Invalidating),
        ("target_gone",           ContinuationSeverity::Invalidating),
        ("target_entity_changed", ContinuationSeverity::Invalidating),
        ("target_hp_drop",        ContinuationSeverity::Relevant),
        ("target_moved",          ContinuationSeverity::Relevant),
    ];
    for (code, expected) in table {
        assert_eq!(
            classify_mismatch(code, &ctx),
            *expected,
            "classify_mismatch({code:?}) must be unchanged"
        );
    }
}
