//! Repair-affinity bonus modifier (step 8.B).
//!
//! Lifted from `scorer.rs::finalize_scores` (lines 282–294).
//! Logic is byte-for-byte identical: aggregate then clamp to ≥0,
//! modulate by `continue_commitment`, scale by `repair_bonus_scale`.

use super::{ModifierCtx, PlanModifier};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::TurnPlan;

pub struct RepairBonus;
pub static MODIFIER: RepairBonus = RepairBonus;

impl PlanModifier for RepairBonus {
    fn name(&self) -> &'static str {
        "repair_bonus"
    }

    fn modify(&self, _plan: &TurnPlan, ann: &PlanAnnotation, ctx: &ModifierCtx<'_, '_, '_>) -> f32 {
        // Guard: only active when a stored goal is present (legacy line :282).
        if ctx.stage.scoring.last_goal.is_none() {
            return 0.0;
        }

        let bonus_scale = ctx.stage.scoring.world.tuning.thresholds.repair_bonus_scale;
        let continue_commitment = ctx.stage.scoring.need_signals.continue_commitment;
        let affinity = ann.repair_affinity;

        // clamp: aggregate is always additive (legacy line :291).
        let bonus = affinity.aggregate(&ctx.repair_weights).max(0.0);
        bonus * (1.0 + continue_commitment) * bonus_scale
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::NeedSignals;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::modifiers::ModifierCtx;
    use crate::combat::ai::pipeline::StageCtx;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::repair::RepairAffinity;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, UnitBuilder};
    use crate::combat::ai::trade::unit_value;
    use crate::combat::ai::utility::AiWorld;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use std::collections::HashMap;

    fn inert_plan(pos: Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![],
            annotation: Default::default(),
        }
    }

    fn make_stored_goal(
        target: bevy::prelude::Entity,
        pos: Hex,
    ) -> crate::combat::ai::repair::StoredGoalContext {
        use crate::combat::ai::repair::goal::GoalKind;
        use crate::combat::ai::repair::StoredGoalContext;
        StoredGoalContext {
            kind: GoalKind::Finish { target },
            region_anchor: pos,
            region_radius: 2,
            planned_ability: None,
            ttl: 2,
            confidence: 1.0,
            created_round: 1,
            expected_actor_pos: pos,
            actor_hp_at_store: 0,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            target_hp_at_store: 0,
            target_pos_at_store: Hex::ZERO,
        }
    }

    /// Without a stored goal, the modifier returns 0 regardless of affinity.
    #[test]
    fn repair_bonus_zero_when_no_stored_goal() {
        let content = crate::combat::ai::test_helpers::empty_content();
        let difficulty = DifficultyProfile::default();
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let world = AiWorld { content: &content, difficulty: &difficulty, tuning: &content.ai_tuning, crit_fail_chance: 0.0 };

        // last_goal = None (make_scoring_ctx default).
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        assert!(scoring.last_goal.is_none());

        let mut rng = crate::core::rng::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        let actor_value = unit_value(&actor, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &summon_dpr, actor_value, repair_weights };

        // Non-zero affinity to confirm it's NOT applied.
        let mut plan = inert_plan(pos);
        plan.annotation.repair_affinity = RepairAffinity {
            goal_alignment: 1.0,
            region_alignment: 1.0,
            method_alignment: 1.0,
            severity_factor: 1.0,
            ttl_factor: 1.0,
            confidence: 1.0,
        };
        let ann = plan.annotation.clone();

        assert_eq!(MODIFIER.modify(&plan, &ann, &ctx), 0.0);
    }

    /// Pin: bonus = aggregate.max(0) × (1 + continue_commitment) × scale.
    ///
    /// Use goal_alignment=1, rest=0 → combined = 1×goal_w.
    /// Multiply by severity=1, ttl=1, confidence=1 → aggregate = goal_w.
    /// With continue_commitment=0.5, scale=0.4:
    ///   expected = goal_w × 1.0 × (1.0 + 0.5) × 0.4 = goal_w × 0.6
    #[test]
    fn repair_bonus_matches_legacy_formula() {
        let content = crate::combat::ai::test_helpers::empty_content();
        let difficulty = DifficultyProfile::default();
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let world = AiWorld { content: &content, difficulty: &difficulty, tuning: &content.ai_tuning, crit_fail_chance: 0.0 };

        let target_entity = bevy::prelude::Entity::from_raw_u32(42).unwrap();
        let stored_goal = make_stored_goal(target_entity, pos);

        let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        scoring.last_goal = Some(&stored_goal);
        scoring.need_signals = NeedSignals { continue_commitment: 0.5, ..Default::default() };

        let mut rng = crate::core::rng::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let actor_value = unit_value(&actor, world.content);
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &summon_dpr, actor_value, repair_weights };

        let affinity = RepairAffinity {
            goal_alignment: 1.0,
            region_alignment: 0.0,
            method_alignment: 0.0,
            severity_factor: 1.0,
            ttl_factor: 1.0,
            confidence: 1.0,
        };
        let mut plan = inert_plan(pos);
        plan.annotation.repair_affinity = affinity;
        let ann = plan.annotation.clone();

        let aggregate = affinity.aggregate(&repair_weights).max(0.0);
        let continue_commitment = 0.5_f32;
        let bonus_scale = world.tuning.thresholds.repair_bonus_scale;
        let expected = aggregate * (1.0 + continue_commitment) * bonus_scale;

        let got = MODIFIER.modify(&plan, &ann, &ctx);
        assert!(
            (got - expected).abs() < 1e-6,
            "repair_bonus formula mismatch: expected {expected}, got {got}"
        );
    }
}
