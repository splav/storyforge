use crate::combat::ai::intent::IntentKind;
use crate::combat::ai::repair::{
    classify_mismatch, compute_status_delta_engine, MismatchContext, PlanContinuationCheck,
    StatusDelta,
};
use crate::combat::ai::world::snapshot::UnitView;
use crate::combat::ai::world::tags::StatusTagCache;
use crate::game::hex::Hex;
use bevy::prelude::*;

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
    pub actor_statuses_at_capture: Vec<combat_engine::StatusId>,
    /// Where the actor should be on the next tick (destination of the Move).
    pub expected_actor_pos: Hex,
    /// Intent target at plan time, if any (FocusTarget / ApplyCC / ProtectAlly).
    pub target: Option<Entity>,
    pub target_hp: i32,
    pub target_pos: Hex,
}

impl PlanSnapshot {
    pub fn capture(
        actor: UnitView<'_>,
        target: Option<UnitView<'_>>,
        expected_actor_pos: Hex,
    ) -> Self {
        Self {
            actor_hp: actor.hp(),
            actor_rage: actor.pools[combat_engine::PoolKind::Rage]
                .map(|(r, _)| r)
                .unwrap_or(0),
            actor_status_hash: status_hash_engine(&actor.statuses),
            actor_statuses_at_capture: actor.statuses.iter().map(|s| s.id.clone()).collect(),
            expected_actor_pos,
            target: target.map(|t| t.entity()),
            target_hp: target.map(|t| t.hp()).unwrap_or(0),
            target_pos: target.map(|t| t.pos).unwrap_or_default(),
        }
    }

    pub fn mismatch(
        &self,
        actor: UnitView<'_>,
        target: Option<UnitView<'_>>,
    ) -> Option<&'static str> {
        if actor.pos != self.expected_actor_pos {
            return Some("actor_pos_mismatch");
        }
        if actor.hp() < self.actor_hp {
            return Some("actor_hp_drop");
        }
        if actor.pools[combat_engine::PoolKind::Rage]
            .map(|(r, _)| r)
            .unwrap_or(0)
            != self.actor_rage
        {
            return Some("actor_rage_changed");
        }
        if status_hash_engine(&actor.statuses) != self.actor_status_hash {
            return Some("actor_status_changed");
        }
        if let Some(expected) = self.target {
            match target {
                None => return Some("target_gone"),
                Some(t) => {
                    if t.entity() != expected {
                        return Some("target_entity_changed");
                    }
                    if t.hp() < self.target_hp {
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

    /// Structured alternative to [`Self::mismatch`]: `Some(check)` with a
    /// classified severity + reason code, or `None` when state still matches.
    /// `mismatch()` is kept unchanged for replay-fixture/test compatibility.
    pub fn check_continuation(
        &self,
        actor: UnitView<'_>,
        target: Option<UnitView<'_>>,
        status_tags: &StatusTagCache,
    ) -> Option<PlanContinuationCheck> {
        let code = self.mismatch(actor, target)?;
        let severity = if code == "actor_status_changed" {
            let delta =
                compute_status_delta_engine(&self.actor_statuses_at_capture, &actor.statuses);
            let ctx = MismatchContext {
                status_delta: Some(&delta),
                status_tags,
            };
            classify_mismatch(code, &ctx)
        } else {
            let ctx = MismatchContext {
                status_delta: None,
                status_tags,
            };
            classify_mismatch(code, &ctx)
        };
        Some(PlanContinuationCheck {
            severity,
            reason_code: code,
        })
    }

    pub fn mismatch_with_delta(
        &self,
        actor: UnitView<'_>,
        target: Option<UnitView<'_>>,
    ) -> Option<(&'static str, Option<StatusDelta>)> {
        let code = self.mismatch(actor, target)?;
        let delta = if code == "actor_status_changed" {
            Some(compute_status_delta_engine(
                &self.actor_statuses_at_capture,
                &actor.statuses,
            ))
        } else {
            None
        };
        Some((code, delta))
    }
}

/// Stable hash over active status ids + remaining durations.
/// Changes when a status is applied, removed, or ticked down.
/// Public for use by `StoredGoalContext::check_continuation` (step 6.6).
/// Takes an engine `ActiveStatus` slice (e.g. via `UnitView::statuses()`).
pub fn status_hash_engine(statuses: &[combat_engine::state::ActiveStatus]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    let mut pairs: Vec<_> = statuses
        .iter()
        .map(|s| (&s.id, s.rounds_remaining))
        .collect();
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
    /// Goal context from the last chosen plan: set after a Move, cleared on
    /// Cast/EndTurn. Read by repair affinity to bonus next-tick plans that
    /// preserve the same goal.
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
    use crate::combat::ai::test_helpers::{ent, snapshot_from, status_view, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn make_status(id: &str, rounds: u32) -> combat_engine::state::ActiveStatus {
        status_view(id, rounds, 0)
    }

    #[test]
    fn snapshot_matches_unchanged_state() {
        let expected_pos = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, expected_pos)
            .hp(10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0)).build();
        let snap = snapshot_from(vec![actor, target], 1);

        let stored =
            PlanSnapshot::capture(snap.unit(ent(1)).unwrap(), snap.unit(ent(2)), expected_pos);
        assert_eq!(
            stored.mismatch(snap.unit(ent(1)).unwrap(), snap.unit(ent(2))),
            None
        );
    }

    #[test]
    fn snapshot_detects_actor_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor_before = UnitBuilder::new(1, Team::Enemy, pos).hp(10).build();
        let actor_after = UnitBuilder::new(1, Team::Enemy, pos).hp(8).build();

        let snap_before = snapshot_from(vec![actor_before], 1);
        let snap_after = snapshot_from(vec![actor_after], 1);

        let stored = PlanSnapshot::capture(snap_before.unit(ent(1)).unwrap(), None, pos);
        assert_eq!(
            stored.mismatch(snap_after.unit(ent(1)).unwrap(), None),
            Some("actor_hp_drop")
        );
    }

    #[test]
    fn snapshot_detects_actor_status_change() {
        let pos = hex_from_offset(0, 0);
        let actor_clean = UnitBuilder::new(1, Team::Enemy, pos).build();
        let mut actor_debuffed = UnitBuilder::new(1, Team::Enemy, pos).build();
        actor_debuffed.statuses.push(make_status("burn", 2));

        let snap_clean = snapshot_from(vec![actor_clean.clone()], 1);
        let snap_debuffed = snapshot_from(vec![actor_debuffed], 1);

        let stored = PlanSnapshot::capture(snap_clean.unit(ent(1)).unwrap(), None, pos);
        assert_eq!(
            stored.mismatch(snap_debuffed.unit(ent(1)).unwrap(), None),
            Some("actor_status_changed")
        );

        // Inverse: had status, now expired.
        let mut actor_with_status = actor_clean;
        actor_with_status.statuses.push(make_status("burn", 2));
        let actor_cured = UnitBuilder::new(1, Team::Enemy, pos).build();

        let snap_with = snapshot_from(vec![actor_with_status], 1);
        let snap_cured = snapshot_from(vec![actor_cured], 1);

        let stored2 = PlanSnapshot::capture(snap_with.unit(ent(1)).unwrap(), None, pos);
        assert_eq!(
            stored2.mismatch(snap_cured.unit(ent(1)).unwrap(), None),
            Some("actor_status_changed")
        );
    }

    #[test]
    fn snapshot_detects_target_death() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).build();
        let snap = snapshot_from(vec![actor, target], 1);

        let stored = PlanSnapshot::capture(snap.unit(ent(1)).unwrap(), snap.unit(ent(2)), pos);
        // Target gone from snapshot (ally killed it between ticks).
        assert_eq!(
            stored.mismatch(snap.unit(ent(1)).unwrap(), None),
            Some("target_gone")
        );
    }

    #[test]
    fn snapshot_detects_target_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target_before = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0))
            .hp(10)
            .build();
        let target_after = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0))
            .hp(4)
            .build();

        let snap_before = snapshot_from(vec![actor.clone(), target_before], 1);
        let snap_after = snapshot_from(vec![actor, target_after], 1);

        let stored = PlanSnapshot::capture(
            snap_before.unit(ent(1)).unwrap(),
            snap_before.unit(ent(2)),
            pos,
        );
        assert_eq!(
            stored.mismatch(snap_after.unit(ent(1)).unwrap(), snap_after.unit(ent(2))),
            Some("target_hp_drop"),
        );
    }

    #[test]
    fn snapshot_detects_actor_pos_mismatch() {
        let expected = hex_from_offset(3, 0);
        let actual = hex_from_offset(2, 0);
        let actor_at_expected = UnitBuilder::new(1, Team::Enemy, expected).build();
        let actor_at_actual = UnitBuilder::new(1, Team::Enemy, actual).build();

        let snap_expected = snapshot_from(vec![actor_at_expected], 1);
        let snap_actual = snapshot_from(vec![actor_at_actual], 1);

        let stored = PlanSnapshot::capture(snap_expected.unit(ent(1)).unwrap(), None, expected);
        // Actor captured at expected pos, but now at actual (path truncated).
        assert_eq!(
            stored.mismatch(snap_actual.unit(ent(1)).unwrap(), None),
            Some("actor_pos_mismatch")
        );
    }
}
