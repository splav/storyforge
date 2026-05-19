use bevy::prelude::*;
use crate::combat::ai::repair::{classify_mismatch, compute_status_delta, MismatchContext, PlanContinuationCheck, StatusDelta};
use crate::combat::ai::world::snapshot::{ActiveStatusView, UnitSnapshot};
use crate::combat::ai::world::tags::StatusTagCache;
use crate::game::hex::Hex;
use crate::combat::ai::intent::IntentKind;

// ── Plan freeze: stored plan + invalidation snapshot ──────────────────────

/// World state captured when a MoveOnly plan step is committed. Compared with
/// current state on the next tick to detect events (AoO, status changes, target
/// death/movement) that would make the stored plan stale.
#[derive(Debug, Clone)]
pub struct PlanSnapshot {
    pub actor_hp: i32,
    /// Current rage value (+1 from AoO — reliable AoO signal without needing
    /// separate event tracking).
    pub actor_rage: i32,
    /// Stable hash over active status ids + remaining durations. Changes when
    /// a status is applied, removed, or ticked down (debuffs, auras, etc.).
    pub actor_status_hash: u64,
    /// Status ids present on the actor at capture time — used to compute the
    /// diff (added/removed) when `actor_status_changed` fires (step 9.B.3).
    pub actor_statuses_at_capture: Vec<crate::core::StatusId>,
    /// Where the actor should be on the next tick (destination of the Move).
    pub expected_actor_pos: Hex,
    /// Intent target at plan time, if any (FocusTarget / ApplyCC / ProtectAlly).
    pub target: Option<Entity>,
    pub target_hp: i32,
    pub target_pos: Hex,
}

impl PlanSnapshot {
    pub fn capture(
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
        expected_actor_pos: Hex,
    ) -> Self {
        Self {
            actor_hp: actor.hp,
            actor_rage: actor.rage.map(|(r, _)| r).unwrap_or(0),
            actor_status_hash: status_hash(&actor.statuses),
            actor_statuses_at_capture: actor.statuses.iter().map(|s| s.id.clone()).collect(),
            expected_actor_pos,
            target: target.map(|t| t.entity),
            target_hp: target.map(|t| t.hp).unwrap_or(0),
            target_pos: target.map(|t| t.pos).unwrap_or_default(),
        }
    }

    /// Returns `None` when the snapshot still matches current world state, or
    /// `Some(reason_code)` identifying the first detected change.
    pub fn mismatch(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
    ) -> Option<&'static str> {
        if actor.pos != self.expected_actor_pos {
            return Some("actor_pos_mismatch");
        }
        if actor.hp < self.actor_hp {
            return Some("actor_hp_drop");
        }
        if actor.rage.map(|(r, _)| r).unwrap_or(0) != self.actor_rage {
            return Some("actor_rage_changed");
        }
        if status_hash(&actor.statuses) != self.actor_status_hash {
            return Some("actor_status_changed");
        }
        if let Some(expected) = self.target {
            match target {
                None => return Some("target_gone"),
                Some(t) => {
                    if t.entity != expected {
                        return Some("target_entity_changed");
                    }
                    if t.hp < self.target_hp {
                        return Some("target_hp_drop");
                    }
                    if t.pos != self.target_pos {
                        return Some("target_moved");
                    }
                }
            }
        }
        None
    }

    /// Structured alternative to `mismatch()` — returns a `PlanContinuationCheck`
    /// with semantic severity instead of a raw reason code.
    ///
    /// Returns `None` when the snapshot still matches current world state (no
    /// mismatch), or `Some(check)` with a classified severity and the original
    /// reason code for telemetry.
    ///
    /// The original `mismatch()` is preserved unchanged for backward compatibility
    /// with replay fixtures and tests.
    pub fn check_continuation(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
        status_tags: &StatusTagCache,
    ) -> Option<PlanContinuationCheck> {
        let code = self.mismatch(actor, target)?;
        let severity = if code == "actor_status_changed" {
            let delta = compute_status_delta(&self.actor_statuses_at_capture, &actor.statuses);
            let ctx = MismatchContext { status_delta: Some(&delta), status_tags };
            classify_mismatch(code, &ctx)
        } else {
            let ctx = MismatchContext { status_delta: None, status_tags };
            classify_mismatch(code, &ctx)
        };
        Some(PlanContinuationCheck { severity, reason_code: code })
    }

    /// Returns `Some((code, Some(delta)))` when the mismatch is `actor_status_changed`,
    /// or `Some((code, None))` for other codes. Returns `None` when no mismatch.
    ///
    /// The delta is computed using the shared `compute_status_delta` helper (step 9.B.0),
    /// ensuring identical diff logic as `StoredGoalContext::check_continuation`.
    pub fn mismatch_with_delta(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
    ) -> Option<(&'static str, Option<StatusDelta>)> {
        let code = self.mismatch(actor, target)?;
        let delta = if code == "actor_status_changed" {
            Some(compute_status_delta(&self.actor_statuses_at_capture, &actor.statuses))
        } else {
            None
        };
        Some((code, delta))
    }
}

/// Stable hash over active status ids + remaining durations.
/// Changes when a status is applied, removed, or ticked down.
/// Public for use by `StoredGoalContext::check_continuation` (step 6.6).
pub fn status_hash(statuses: &[ActiveStatusView]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    // Sort by id for a deterministic hash regardless of application order.
    let mut pairs: Vec<_> = statuses.iter().map(|s| (&s.id, s.rounds_remaining)).collect();
    pairs.sort_by_key(|(id, _)| id.0.as_str());
    for (id, rounds) in pairs {
        id.hash(&mut h);
        rounds.hash(&mut h);
    }
    h.finish()
}

// ── Persistent AI memory ───────────────────────────────────────────────────

#[derive(Component, Default)]
pub struct AiMemory {
    pub last_intent: Option<IntentKind>,
    pub last_target: Option<Entity>,
    pub turns_committed: u8,
    /// Step 6.1/6.6: goal context extracted from the last chosen plan.
    /// Set after a Move decision; cleared on Cast / EndTurn.
    /// Used by repair affinity (6.2+) to bonus fresh plans that preserve
    /// the same goal on the next tick.
    ///
    /// Replaces the removed `last_plan: Option<StoredPlan>` (step 6.6).
    pub last_goal: Option<crate::combat::ai::repair::StoredGoalContext>,
    /// HP ratio of the actor at the time of the previous decision. `None` until
    /// the actor takes its first turn — then read in step 3.1 producer to compute
    /// `recent_damage_taken`.
    pub hp_ratio_at_last_turn: Option<f32>,
    /// True if the actor's previous intent was a defensive/survival one
    /// (`ProtectSelf` or `LastStand`). Read in step 3.1 to dampen `self_preserve`
    /// when no fresh damage came in.
    pub last_turn_was_defensive: bool,
    /// Number of consecutive turns the actor has been in the low-HP zone
    /// (`hp_pct < tuning.thresholds.low_hp_zone_threshold`). Read in step 3.1
    /// as a secondary input to `self_preserve`.
    pub turns_in_low_hp: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::world::snapshot::{ActiveStatusView, BattleSnapshot};
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::core::StatusId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn make_status(id: &str, rounds: u32) -> ActiveStatusView {
        ActiveStatusView { id: StatusId::from(id), rounds_remaining: rounds, dot_per_tick: 0 }
    }

    #[test]
    fn snapshot_matches_unchanged_state() {
        let expected_pos = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, expected_pos).hp(10).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0)).build();
        let _snap = snapshot_from(vec![actor.clone(), target.clone()], 1);

        let stored = PlanSnapshot::capture(&actor, Some(&target), expected_pos);
        assert_eq!(stored.mismatch(&actor, Some(&target)), None);
    }

    #[test]
    fn snapshot_detects_actor_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor_before = UnitBuilder::new(1, Team::Enemy, pos).hp(10).build();
        let actor_after = UnitBuilder::new(1, Team::Enemy, pos).hp(8).build(); // AoO hit
        let _snap = snapshot_from(vec![actor_before.clone()], 1);

        let stored = PlanSnapshot::capture(&actor_before, None, pos);
        assert_eq!(stored.mismatch(&actor_after, None), Some("actor_hp_drop"));
    }

    #[test]
    fn snapshot_detects_actor_status_change() {
        let pos = hex_from_offset(0, 0);
        let mut actor_clean = UnitBuilder::new(1, Team::Enemy, pos).build();
        let mut actor_debuffed = actor_clean.clone();
        actor_debuffed.statuses.push(make_status("burn", 2));

        let stored = PlanSnapshot::capture(&actor_clean, None, pos);
        assert_eq!(stored.mismatch(&actor_debuffed, None), Some("actor_status_changed"));

        // Inverse: had status, now expired.
        actor_clean.statuses.push(make_status("burn", 2));
        let stored2 = PlanSnapshot::capture(&actor_clean, None, pos);
        let mut actor_cured = actor_clean.clone();
        actor_cured.statuses.clear();
        assert_eq!(stored2.mismatch(&actor_cured, None), Some("actor_status_changed"));
    }

    #[test]
    fn snapshot_detects_target_death() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).build();

        let stored = PlanSnapshot::capture(&actor, Some(&target), pos);
        // Target gone from snapshot (ally killed it between ticks).
        assert_eq!(stored.mismatch(&actor, None), Some("target_gone"));
    }

    #[test]
    fn snapshot_detects_target_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target_before = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).hp(10).build();
        let target_after = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).hp(4).build();

        let stored = PlanSnapshot::capture(&actor, Some(&target_before), pos);
        assert_eq!(stored.mismatch(&actor, Some(&target_after)), Some("target_hp_drop"));
    }

    #[test]
    fn snapshot_detects_actor_pos_mismatch() {
        let expected = hex_from_offset(3, 0);
        let actual = hex_from_offset(2, 0); // AoO truncated the path
        let actor_at_expected = UnitBuilder::new(1, Team::Enemy, expected).build();
        let actor_at_actual = UnitBuilder::new(1, Team::Enemy, actual).build();

        let stored = PlanSnapshot::capture(&actor_at_expected, None, expected);
        // Actor captured at expected pos, but now at actual (path truncated).
        assert_eq!(stored.mismatch(&actor_at_actual, None), Some("actor_pos_mismatch"));
    }
}
