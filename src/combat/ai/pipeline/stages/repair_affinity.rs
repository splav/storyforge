//! RepairAffinityStage — step 7.3.
//!
//! Moves the inline repair-affinity loop from `pick_action` body into a typed
//! `PlanStage`. Populates `pool.annotations[i].repair_affinity` for every plan
//! when a stored goal exists; no-op otherwise.
//!
//! The bonus application itself (`finalize_scores`) is unchanged — this stage
//! only populates the annotation field that `finalize_scores` already reads.

use crate::combat::ai::repair::compute_repair_affinity;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

pub struct RepairAffinityStage;

impl PlanStage for RepairAffinityStage {
    fn name(&self) -> &'static str {
        "repair_affinity"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(stored_goal) = ctx.scoring.last_goal else { return };

        let severity = {
            let actor_snap = ctx.scoring.active;
            let target_snap = stored_goal
                .target_entity()
                .and_then(|t| ctx.scoring.snap.unit_snapshot(t));
            stored_goal
                .check_continuation(actor_snap, target_snap, ctx.scoring.world.status_tags)
                .map(|c| c.severity)
        };

        let current_round = ctx.scoring.snap.round;

        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            ann.repair_affinity = compute_repair_affinity(
                ctx.intent,
                &plan.steps,
                plan.final_pos,
                stored_goal,
                severity,
                current_round,
            );
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::memory::goal::{GoalKind, StoredGoalContext};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, PoolBuilder, UnitBuilder, ent,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn stored_finish(target: bevy::prelude::Entity, round: u32) -> StoredGoalContext {
        StoredGoalContext {
            kind: GoalKind::Finish { target },
            region_anchor: hex_from_offset(0, 0),
            region_radius: 2,
            planned_ability: None,
            ttl: 3,
            confidence: 1.0,
            created_round: round,
            expected_actor_pos: hex_from_offset(0, 0),
            actor_hp_at_store: 10,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            actor_statuses_at_store: vec![],
            target_hp_at_store: 8,
            target_pos_at_store: hex_from_offset(2, 0),
        }
    }

    /// Run stage with an optional stored goal.
    /// Cannot use StageTestHarness: needs scoring.last_goal injection which the
    /// harness doesn't expose (last_goal lives in ScoringCtx, not on StageCtx fields).
    /// TODO: migrate to StageTestHarness in Phase 5 if last_goal injection is added.
    fn run_stage_with_goal(
        plans: Vec<TurnPlan>,
        intent: TacticalIntent,
        snap: &BattleSnapshot,
        actor: &crate::combat::ai::world::snapshot::UnitSnapshot,
        last_goal: Option<&StoredGoalContext>,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let mut scoring = make_scoring_ctx(&world, snap, &maps, &reservations, actor);
        scoring.last_goal = last_goal;
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            intent,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );
        let n = plans.len();
        let mut pool = PoolBuilder::new(plans).scores(&vec![0.5; n]).build();
        RepairAffinityStage.apply(&mut pool, &mut ctx);
        pool
    }

    #[test]
    fn repair_affinity_stage_no_op_when_no_stored_goal() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2–4. Act (helper handles setup) ──
        let pool = run_stage_with_goal(
            plans,
            TacticalIntent::Reposition,
            &snap,
            &actor,
            None,
        );

        // ── 5. Assert ──
        // No stored goal → annotations stay at default (all-zero).
        for ann in &pool.annotations {
            let aff = &ann.repair_affinity;
            assert_eq!(aff.goal_alignment, 0.0, "no stored goal → goal_alignment = 0");
            assert_eq!(aff.confidence, 0.0, "no stored goal → confidence = 0");
        }
    }

    #[test]
    fn repair_affinity_stage_populates_annotation() {
        // ── 1. Test data ──
        let target = ent(2);
        let pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target_unit = UnitBuilder::new(2, Team::Player, target_pos).hp(8).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target_unit], 1);
        let stored = stored_finish(target, 0);

        // Plan that pursues the same FocusTarget → should get goal_alignment > 0
        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: crate::core::AbilityId::from("attack"),
                target,
                target_pos,
            }],
            final_pos: pos,
            ..TurnPlan::default()
        };

        // ── 2–4. Act (helper handles setup) ──
        let pool = run_stage_with_goal(
            vec![plan],
            TacticalIntent::FocusTarget { target },
            &snap,
            &actor,
            Some(&stored),
        );

        // ── 5. Assert ──
        let aff = &pool.annotations[0].repair_affinity;
        assert!(aff.goal_alignment > 0.0, "same-target FocusTarget plan must have goal_alignment > 0");
        assert!(aff.confidence > 0.0, "confidence must be copied from stored goal");
    }
}
