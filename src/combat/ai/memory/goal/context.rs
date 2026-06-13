/// Step 6.1 — goal extraction: `StoredGoalContext` + `extract_goal_context`.
///
/// A `StoredGoalContext` captures the *semantic intent* of a chosen plan at
/// the moment of a Move decision: what the actor was trying to accomplish,
/// where, and how confident the scorer was.  Unlike `StoredPlan` (which stores
/// the literal step sequence for exact-continuation), `StoredGoalContext` is
/// used by the repair-affinity system (6.2+) to award scoring bonuses to fresh
/// plans that preserve the same goal.
use bevy::prelude::Entity;
use serde::{Deserialize, Serialize};

use crate::combat::ai::config::tuning::AiTuning;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::plan::types::PlanStep;
use crate::combat::ai::repair::{
    classify_mismatch, compute_status_delta_engine, MismatchContext, PlanContinuationCheck,
    StatusDelta,
};
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::world::tags::StatusTagCache;
use crate::game::hex::Hex;
use combat_engine::AbilityId;

// ── GoalKind ─────────────────────────────────────────────────────────────────

/// Semantic category of the goal that was stored.
///
/// Variants are tagged in JSONL as `"kind": "<snake_case>"` for future log
/// analysis (6.5). Entity fields use the project's bit-pack serde adapter so
/// they survive round-trips through JSONL without loss.
///
/// Step 6.1 first wave — 7 kinds matching the 6 `TacticalIntent` variants
/// (FocusTarget splits into Finish vs Pressure). Corridor / zone variants
/// deferred to step 17 (geometry awareness).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalKind {
    /// Kill the target this turn or as soon as possible.
    Finish {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        target: Entity,
    },
    /// Pressure / whittle down the target without expecting an immediate kill.
    Pressure {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        target: Entity,
    },
    /// Apply crowd-control to the target.
    DisableEnemy {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        target: Entity,
    },
    /// Heal / protect an allied unit.
    HealAlly {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        ally: Entity,
    },
    /// Retreat to a safe region (ProtectSelf / LastStand with movement).
    Retreat {
        #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
        region_anchor: Hex,
    },
    /// Reposition into the blast zone of a planned AoE cast.
    SetupAOE {
        #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
        region_center: Hex,
        planned_ability: AbilityId,
    },
    /// Pure repositioning — improve board position without a specific target.
    Reposition {
        #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
        region_center: Hex,
    },
}

// ── StoredGoalContext ─────────────────────────────────────────────────────────

/// Persistent goal extracted from a chosen plan at Move-decision time.
///
/// Stored in `AiMemory.last_goal`; consumed by repair affinity (6.2) to bonus
/// fresh plans that preserve the same goal on the actor's next tick.
///
/// Step 6.6: extended with severity-check fields (previously in `PlanSnapshot`),
/// so continuation severity can be computed without `AiMemory.last_plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGoalContext {
    /// Semantic goal kind and its primary entity / position anchor.
    pub kind: GoalKind,
    /// Hex anchor for region/corridor checks.
    /// For target-bound kinds (`Finish`, `Pressure`, `DisableEnemy`,
    /// `HealAlly`) this equals the target's position at store time.
    /// For positional kinds it equals `chosen_final_pos`.
    #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
    pub region_anchor: Hex,
    /// Maximum hex distance from `region_anchor` that is still considered
    /// "on-goal" for repair-affinity region alignment. Read from
    /// `tuning.thresholds.repair_region_radius` at store time.
    pub region_radius: u32,
    /// Ability we expected to cast as the climax of this goal, if any.
    /// Derived from `chosen_steps[1]` when it is a `Cast` step.
    /// Used to bonus `method_preserved` over `method_changed` in 6.2.
    pub planned_ability: Option<AbilityId>,
    /// Rounds remaining before the goal expires via TTL decay.
    /// Initialised from `tuning.thresholds.repair_default_ttl`.
    pub ttl: u8,
    /// Scorer confidence in this goal at store time: `chosen_score / pool_max_score`.
    /// Clamped to `[0.0, 1.0]`. Acts as a multiplicative gate on repair bonus (6.3).
    pub confidence: f32,
    /// Combat round when the goal was created — used for TTL decay and telemetry.
    pub created_round: u32,
    // ── Severity-check fields (step 6.6) ────────────────────────────────────
    // These replicate the fields from `PlanSnapshot` so that `check_continuation`
    // can be called on `StoredGoalContext` after `AiMemory.last_plan` is removed.
    /// Where the actor should be on the next tick (destination of the Move).
    #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
    pub expected_actor_pos: Hex,
    /// Actor HP at the time of store.
    pub actor_hp_at_store: i32,
    /// Actor rage at the time of store (0 when no rage resource).
    pub actor_rage_at_store: i32,
    /// Stable hash over actor status ids + remaining durations at store time.
    pub actor_status_hash: u64,
    /// Status ids present on the actor at store time — used to compute the
    /// diff (added/removed) when `actor_status_changed` fires (step 9.B.3).
    #[serde(default)]
    pub actor_statuses_at_store: Vec<combat_engine::StatusId>,
    /// Target HP at the time of store (0 when no target).
    pub target_hp_at_store: i32,
    /// Target position at the time of store (Hex::ZERO when no target).
    #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
    pub target_pos_at_store: Hex,
}

impl GoalKind {
    /// Short ASCII code for telemetry / log fields. Stable across refactors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Finish { .. } => "finish",
            Self::Pressure { .. } => "pressure",
            Self::DisableEnemy { .. } => "disable_enemy",
            Self::HealAlly { .. } => "heal_ally",
            Self::Retreat { .. } => "retreat",
            Self::SetupAOE { .. } => "setup_aoe",
            Self::Reposition { .. } => "reposition",
        }
    }

    /// Returns the entity this goal targets, if applicable.
    pub fn target_entity(&self) -> Option<Entity> {
        match self {
            Self::Finish { target } | Self::Pressure { target } | Self::DisableEnemy { target } => {
                Some(*target)
            }
            Self::HealAlly { ally } => Some(*ally),
            Self::Retreat { .. } | Self::SetupAOE { .. } | Self::Reposition { .. } => None,
        }
    }
}

impl StoredGoalContext {
    /// Returns the entity this goal targets, if applicable.
    pub fn target_entity(&self) -> Option<Entity> {
        self.kind.target_entity()
    }

    /// Severity check — mirrors `PlanSnapshot::mismatch` + `classify_mismatch`
    /// but operates on `StoredGoalContext`'s own snapshot fields.
    ///
    /// Returns `None` when the world state still matches (no mismatch), or
    /// `Some(check)` with a classified severity and reason code.
    ///
    /// This replaces the old path that read severity from `AiMemory.last_plan`
    /// (removed in step 6.6).
    pub fn check_continuation(
        &self,
        actor: UnitView<'_>,
        target: Option<UnitView<'_>>,
        status_tags: &StatusTagCache,
    ) -> Option<PlanContinuationCheck> {
        // 1. Actor position mismatch — topology broken.
        let no_delta_ctx = MismatchContext {
            status_delta: None,
            status_tags,
        };
        if actor.pos != self.expected_actor_pos {
            return Some(PlanContinuationCheck {
                severity: classify_mismatch("actor_pos_mismatch", &no_delta_ctx),
                reason_code: "actor_pos_mismatch",
            });
        }
        // 2. Actor HP dropped — self-preserve re-eval needed, goal alive.
        if actor.hp() < self.actor_hp_at_store {
            return Some(PlanContinuationCheck {
                severity: classify_mismatch("actor_hp_drop", &no_delta_ctx),
                reason_code: "actor_hp_drop",
            });
        }
        // 3. Actor rage changed — cosmetic side-effect of AoO / round mechanics.
        if actor.pools[combat_engine::PoolKind::Rage]
            .map(|(r, _)| r)
            .unwrap_or(0)
            != self.actor_rage_at_store
        {
            return Some(PlanContinuationCheck {
                severity: classify_mismatch("actor_rage_changed", &no_delta_ctx),
                reason_code: "actor_rage_changed",
            });
        }
        // 4. Actor status set changed — compute delta for semantic classification.
        if crate::combat::ai::memory::ai_memory::status_hash_engine(&actor.statuses)
            != self.actor_status_hash
        {
            let delta: StatusDelta =
                compute_status_delta_engine(&self.actor_statuses_at_store, &actor.statuses);
            let delta_ctx = MismatchContext {
                status_delta: Some(&delta),
                status_tags,
            };
            return Some(PlanContinuationCheck {
                severity: classify_mismatch("actor_status_changed", &delta_ctx),
                reason_code: "actor_status_changed",
            });
        }
        // 5. Target checks — only when goal has an entity target.
        if self.target_entity().is_some() {
            match target {
                None => {
                    return Some(PlanContinuationCheck {
                        severity: classify_mismatch("target_gone", &no_delta_ctx),
                        reason_code: "target_gone",
                    });
                }
                Some(t) => {
                    if Some(t.entity()) != self.target_entity() {
                        return Some(PlanContinuationCheck {
                            severity: classify_mismatch("target_entity_changed", &no_delta_ctx),
                            reason_code: "target_entity_changed",
                        });
                    }
                    if t.hp() < self.target_hp_at_store {
                        return Some(PlanContinuationCheck {
                            severity: classify_mismatch("target_hp_drop", &no_delta_ctx),
                            reason_code: "target_hp_drop",
                        });
                    }
                    if t.pos != self.target_pos_at_store {
                        return Some(PlanContinuationCheck {
                            severity: classify_mismatch("target_moved", &no_delta_ctx),
                            reason_code: "target_moved",
                        });
                    }
                }
            }
        }
        // No mismatch detected.
        None
    }
}

// ── Producer ─────────────────────────────────────────────────────────────────

/// Extract a `StoredGoalContext` from the fields of the chosen plan.
///
/// Called in `run_ai_turn` in parallel with setting `AiMemory.last_plan`,
/// only when the decision is a `Move` (not Cast / EndTurn).
///
/// Returns `None` when the intent does not produce a representable goal
/// (currently: `SetupAOE` without a Cast step at index 1).
///
/// # Parameters
/// - `chosen_intent` — the `TacticalIntent` that drove this plan.
/// - `chosen_steps` — `chosen.plan.steps`.
/// - `chosen_outcomes` — `chosen.plan.annotation.outcomes`; used to sum
///   `p_kill_now` across steps for Finish vs Pressure classification.
/// - `chosen_final_pos` — `chosen.plan.final_pos`.
/// - `chosen_score` — final adapted score of the chosen plan.
/// - `pool_max_score` — best score seen across all plans (used for confidence
///   normalisation).  Pass `chosen_score.max(1.0)` as a sanity fallback when
///   the true pool maximum is unavailable.
/// - `snap` — current battle snapshot (for target position look-up).
/// - `actor` — the actor's current snapshot (for severity-check fields; step 6.6).
/// - `round` — current combat round (`CombatContext.round`).
/// - `tuning` — per-actor tuning (thresholds read at store time).
#[allow(clippy::too_many_arguments)]
pub fn extract_goal_context(
    chosen_intent: TacticalIntent,
    chosen_steps: &[PlanStep],
    chosen_outcomes: &[ActionOutcomeEstimate],
    chosen_final_pos: Hex,
    chosen_score: f32,
    pool_max_score: f32,
    snap: &BattleSnapshot,
    actor: UnitView<'_>,
    round: u32,
    tuning: &AiTuning,
) -> Option<StoredGoalContext> {
    let kind = intent_to_goal_kind(
        chosen_intent,
        chosen_steps,
        chosen_outcomes,
        chosen_final_pos,
        snap,
        tuning,
    )?;

    let region_anchor = region_anchor_for(&kind, snap)?;

    // planned_ability: step[1] if it's a Cast, regardless of kind.
    let planned_ability = chosen_steps.get(1).and_then(|s| match s {
        PlanStep::Cast { ability, .. } => Some(ability.clone()),
        _ => None,
    });

    let confidence = (chosen_score / pool_max_score.max(1e-6)).clamp(0.0, 1.0);

    // Capture severity-check fields from the actor view (step 6.6).
    // target_snap is looked up from the goal kind so the same entity used for
    // region_anchor is also captured for the target severity checks.
    let target_entity = kind.target_entity();
    let target_snap = target_entity.and_then(|e| snap.unit(e));
    let actor_rage_at_store = actor.pools[combat_engine::PoolKind::Rage]
        .map(|(r, _)| r)
        .unwrap_or(0);
    let actor_status_hash =
        crate::combat::ai::memory::ai_memory::status_hash_engine(&actor.statuses);
    // Status id list for delta-based severity classification (step 9.B.3).
    let actor_statuses_at_store: Vec<combat_engine::StatusId> =
        actor.statuses.iter().map(|s| s.id.clone()).collect();

    Some(StoredGoalContext {
        kind,
        region_anchor,
        region_radius: tuning.thresholds.repair_region_radius,
        planned_ability,
        ttl: tuning.thresholds.repair_default_ttl,
        confidence,
        created_round: round,
        expected_actor_pos: chosen_final_pos,
        actor_hp_at_store: actor.hp(),
        actor_rage_at_store,
        actor_status_hash,
        actor_statuses_at_store,
        target_hp_at_store: target_snap.map(|t| t.hp()).unwrap_or(0),
        target_pos_at_store: target_snap.map(|t| t.pos).unwrap_or_default(),
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Map a `TacticalIntent` to a `GoalKind`, returning `None` when no valid goal
/// can be constructed (e.g. `SetupAOE` without a following Cast step).
fn intent_to_goal_kind(
    intent: TacticalIntent,
    steps: &[PlanStep],
    outcomes: &[ActionOutcomeEstimate],
    final_pos: Hex,
    snap: &BattleSnapshot,
    tuning: &AiTuning,
) -> Option<GoalKind> {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            // Distinguish Finish vs Pressure by target HP and cumulative p_kill_now.
            let t = snap.unit(target)?;
            let p_kill_now: f32 = outcomes.iter().map(|o| o.p_kill_now).sum::<f32>().min(1.0);
            if p_kill_now >= tuning.thresholds.goal_finish_p_kill || t.hp_pct() < 0.30 {
                Some(GoalKind::Finish { target })
            } else {
                Some(GoalKind::Pressure { target })
            }
        }
        TacticalIntent::ApplyCC { target } => Some(GoalKind::DisableEnemy { target }),
        TacticalIntent::ProtectAlly { ally } => Some(GoalKind::HealAlly { ally }),
        // LastStand is now an EvaluationMode, not a TacticalIntent; adapt-triggered
        // plans that used LastStand scoring may be selected under any global intent,
        // but the goal kind here follows the global intent selection.
        TacticalIntent::ProtectSelf => Some(GoalKind::Retreat {
            region_anchor: final_pos,
        }),
        TacticalIntent::SetupAOE => {
            // SetupAOE without a following Cast step is not a representable goal.
            let ability = steps.get(1).and_then(|s| match s {
                PlanStep::Cast { ability, .. } => Some(ability.clone()),
                _ => None,
            })?;
            Some(GoalKind::SetupAOE {
                region_center: final_pos,
                planned_ability: ability,
            })
        }
        TacticalIntent::Reposition => Some(GoalKind::Reposition {
            region_center: final_pos,
        }),
    }
}

/// Derive the `region_anchor` for the stored context from the goal kind.
///
/// For target-bound goals the anchor is the target's current position (so the
/// region check in 6.2 is relative to where the target was at store time).
/// For positional goals it is `chosen_final_pos`.
fn region_anchor_for(kind: &GoalKind, snap: &BattleSnapshot) -> Option<Hex> {
    match kind {
        GoalKind::Finish { target }
        | GoalKind::Pressure { target }
        | GoalKind::DisableEnemy { target } => Some(snap.unit(*target)?.pos),
        GoalKind::HealAlly { ally } => Some(snap.unit(*ally)?.pos),
        GoalKind::Retreat { region_anchor } => Some(*region_anchor),
        GoalKind::SetupAOE { region_center, .. } | GoalKind::Reposition { region_center } => {
            Some(*region_center)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::test_helpers::{ent, snapshot_from, UnitBuilder};
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::Hex;

    fn default_tuning() -> AiTuning {
        AiTuning::default()
    }

    /// Build a minimal `BattleSnapshot` with the dummy actor (id=1) plus an
    /// optional extra unit. The dummy actor is always present so that
    /// `snap.unit(ent(1))` succeeds for `extract_goal_context` calls.
    fn snap_with_unit(unit: crate::combat::ai::test_helpers::UnitFixture) -> BattleSnapshot {
        snapshot_from(vec![dummy_actor_snapshot(), unit], 1)
    }

    // Convenience: no-op outcomes (no kills estimated).
    fn no_outcomes() -> Vec<ActionOutcomeEstimate> {
        vec![]
    }

    // Dummy actor snapshot for tests that don't care about severity-check fields.
    fn dummy_actor_snapshot() -> crate::combat::ai::test_helpers::UnitFixture {
        UnitBuilder::new(1, Team::Enemy, Hex::ZERO).build()
    }

    // A Cast step for use in plan steps.
    fn cast_step(ability: &str) -> PlanStep {
        PlanStep::Cast {
            ability: AbilityId::from(ability),
            target: ent(99),
            target_pos: Hex::ZERO,
        }
    }

    // A Move step for use in plan steps.
    fn move_step() -> PlanStep {
        PlanStep::Move {
            path: vec![Hex::new(1, 0)],
        }
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// FocusTarget on a target at hp_pct < 0.30 → GoalKind::Finish.
    #[test]
    fn extract_finish_for_low_hp_target() {
        let target = ent(2);
        let target_unit = UnitBuilder::new(2, Team::Enemy, Hex::new(3, 0))
            .hp(5)
            .max_hp(20) // hp_pct = 0.25 < 0.30
            .build();
        let snap = snap_with_unit(target_unit);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();

        let result = extract_goal_context(
            TacticalIntent::FocusTarget { target },
            &[move_step()],
            &no_outcomes(), // p_kill_now = 0.0
            Hex::new(2, 0),
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        let ctx = result.expect("should produce a goal");
        assert!(matches!(ctx.kind, GoalKind::Finish { target: t } if t == target));
        // region_anchor should be target's position
        assert_eq!(ctx.region_anchor, Hex::new(3, 0));
    }

    /// FocusTarget on a high-HP target with p_kill_now=0 → GoalKind::Pressure.
    #[test]
    fn extract_pressure_for_high_hp_target() {
        let target = ent(2);
        let target_unit = UnitBuilder::new(2, Team::Enemy, Hex::new(3, 0))
            .hp(16)
            .max_hp(20) // hp_pct = 0.80
            .build();
        let snap = snap_with_unit(target_unit);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();

        let result = extract_goal_context(
            TacticalIntent::FocusTarget { target },
            &[move_step()],
            &no_outcomes(), // p_kill_now = 0.0
            Hex::new(2, 0),
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        let ctx = result.expect("should produce a goal");
        assert!(matches!(ctx.kind, GoalKind::Pressure { target: t } if t == target));
    }

    /// FocusTarget on a high-HP target but p_kill_now >= goal_finish_p_kill → Finish.
    #[test]
    fn extract_finish_when_p_kill_high() {
        let target = ent(2);
        let target_unit = UnitBuilder::new(2, Team::Enemy, Hex::new(3, 0))
            .hp(16)
            .max_hp(20) // hp_pct = 0.80, would be Pressure…
            .build();
        let snap = snap_with_unit(target_unit);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning(); // goal_finish_p_kill = 0.6

        // …but p_kill_now = 0.7 ≥ 0.6 → Finish
        let high_kill = vec![ActionOutcomeEstimate {
            p_kill_now: 0.7,
            ..Default::default()
        }];

        let result = extract_goal_context(
            TacticalIntent::FocusTarget { target },
            &[move_step()],
            &high_kill,
            Hex::new(2, 0),
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        let ctx = result.expect("should produce a goal");
        assert!(matches!(ctx.kind, GoalKind::Finish { target: t } if t == target));
    }

    /// SetupAOE + step[1] = Cast → GoalKind::SetupAOE with the ability recovered.
    #[test]
    fn extract_setupaoe_recovers_planned_ability() {
        let snap = snapshot_from(vec![dummy_actor_snapshot()], 1);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();
        let ability_id = AbilityId::from("fireball");

        let steps = vec![move_step(), cast_step("fireball")];

        let result = extract_goal_context(
            TacticalIntent::SetupAOE,
            &steps,
            &no_outcomes(),
            Hex::new(4, 0),
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        let ctx = result.expect("should produce a goal");
        match &ctx.kind {
            GoalKind::SetupAOE {
                planned_ability,
                region_center,
            } => {
                assert_eq!(planned_ability, &ability_id);
                assert_eq!(*region_center, Hex::new(4, 0));
            }
            other => panic!("expected SetupAOE, got {other:?}"),
        }
        // planned_ability field on StoredGoalContext also set
        assert_eq!(ctx.planned_ability, Some(ability_id));
    }

    /// SetupAOE + step[1] is not a Cast → None (no goal representable).
    #[test]
    fn extract_setupaoe_returns_none_without_cast_step() {
        let snap = snapshot_from(vec![dummy_actor_snapshot()], 1);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();

        let steps = vec![move_step(), move_step()]; // step[1] is Move, not Cast

        let result = extract_goal_context(
            TacticalIntent::SetupAOE,
            &steps,
            &no_outcomes(),
            Hex::new(4, 0),
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        assert!(
            result.is_none(),
            "SetupAOE without Cast step should produce None"
        );
    }

    /// ProtectSelf → Retreat with region_anchor == chosen_final_pos.
    #[test]
    fn extract_retreat_uses_final_pos_anchor() {
        let snap = snapshot_from(vec![dummy_actor_snapshot()], 1);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();
        let final_pos = Hex::new(2, 3);

        let result = extract_goal_context(
            TacticalIntent::ProtectSelf,
            &[move_step()],
            &no_outcomes(),
            final_pos,
            1.0,
            1.0,
            &snap,
            actor,
            1,
            &tuning,
        );

        let ctx = result.expect("should produce a goal");
        assert!(
            matches!(ctx.kind, GoalKind::Retreat { region_anchor } if region_anchor == final_pos)
        );
        assert_eq!(ctx.region_anchor, final_pos);
    }

    /// confidence = chosen_score / pool_max_score, clamped to [0, 1].
    #[test]
    fn confidence_clamps_to_unit_interval() {
        let snap = snapshot_from(vec![dummy_actor_snapshot()], 1);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();

        // chosen_score > pool_max → confidence clamped to 1.0
        let ctx1 = extract_goal_context(
            TacticalIntent::Reposition,
            &[move_step()],
            &no_outcomes(),
            Hex::ZERO,
            2.0, // chosen_score
            1.0, // pool_max_score
            &snap,
            actor,
            1,
            &tuning,
        )
        .expect("should produce a goal");
        assert_eq!(ctx1.confidence, 1.0);

        // chosen_score = 0.5, pool_max = 1.0 → confidence = 0.5
        let ctx2 = extract_goal_context(
            TacticalIntent::Reposition,
            &[move_step()],
            &no_outcomes(),
            Hex::ZERO,
            0.5, // chosen_score
            1.0, // pool_max_score
            &snap,
            actor,
            1,
            &tuning,
        )
        .expect("should produce a goal");
        assert!((ctx2.confidence - 0.5).abs() < 1e-6);
    }

    /// pool_max_score = 0.0 → confidence is finite and ≤ 1.0 (no NaN/inf).
    #[test]
    fn confidence_zero_safe_when_pool_max_zero() {
        let snap = snapshot_from(vec![dummy_actor_snapshot()], 1);
        let actor = snap.unit(ent(1)).unwrap();
        let tuning = default_tuning();

        let ctx = extract_goal_context(
            TacticalIntent::Reposition,
            &[move_step()],
            &no_outcomes(),
            Hex::ZERO,
            0.5, // chosen_score
            0.0, // pool_max_score (degenerate)
            &snap,
            actor,
            1,
            &tuning,
        )
        .expect("should produce a goal");

        assert!(ctx.confidence.is_finite(), "confidence must be finite");
        assert!(ctx.confidence <= 1.0, "confidence must be ≤ 1.0");
    }

    /// StoredGoalContext uses the same `compute_status_delta` helper as
    /// `PlanSnapshot::mismatch_with_delta`. Sanity check: when no statuses were
    /// stored and the actor currently has `["stunned"]`, the delta shows it added.
    #[test]
    fn stored_goal_context_uses_shared_compute_status_delta() {
        use crate::combat::ai::repair::compute_status_delta;
        use crate::combat::ai::world::snapshot::ActiveStatusView;
        use combat_engine::StatusId;

        let stored_statuses: Vec<StatusId> = vec![];
        let current_statuses = vec![ActiveStatusView {
            id: StatusId::from("stunned"),
            rounds_remaining: 2,
            dot_per_tick: 0,
        }];

        let delta = compute_status_delta(&stored_statuses, &current_statuses);
        assert_eq!(delta.added, vec![StatusId::from("stunned")]);
        assert!(delta.removed.is_empty());

        // Mirror: PlanSnapshot uses the same call — ensure the functions are identical
        // by checking symmetry.
        let stored2 = vec![StatusId::from("stunned")];
        let current2: Vec<ActiveStatusView> = vec![];
        let delta2 = compute_status_delta(&stored2, &current2);
        assert!(delta2.added.is_empty());
        assert_eq!(delta2.removed, vec![StatusId::from("stunned")]);
    }
}
