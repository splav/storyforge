//! Repair-affinity bonus modifier (step 8.B).
//!
//! Lifted from `scorer.rs::finalize_scores` (lines 282–294).
//! Logic is byte-for-byte identical: aggregate then clamp to ≥0,
//! modulate by `continue_commitment`, scale by `repair_bonus_scale`.

use super::{ModifierCtx, PlanModifier};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::TurnPlan;

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
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::orchestration::AiWorld;
    use crate::combat::ai::pipeline::stages::modifiers::ModifierCtx;
    use crate::combat::ai::pipeline::StageCtx;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::repair::RepairAffinity;
    use crate::combat::ai::scoring::trade::unit_value;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, UnitBuilder,
    };
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use std::collections::HashMap;

    fn inert_plan(pos: Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            final_pos: pos,
            ..TurnPlan::default()
        }
    }

    fn make_stored_goal(
        target: bevy::prelude::Entity,
        pos: Hex,
    ) -> crate::combat::ai::repair::StoredGoalContext {
        use crate::combat::ai::memory::goal::GoalKind;
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
            actor_statuses_at_store: vec![],
            target_hp_at_store: 0,
            target_pos_at_store: Hex::ZERO,
        }
    }

    fn make_world_and_actor<'a>(
        content: &'a crate::content::content_view::ContentView,
        difficulty: &'a DifficultyProfile,
        pos: Hex,
    ) -> (
        AiWorld<'a>,
        crate::combat::ai::world::snapshot::UnitSnapshot,
        BattleSnapshot,
    ) {
        let world = AiWorld {
            content,
            difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(),
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
        };
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        (world, actor, snap)
    }

    /// Without a stored goal, the modifier returns 0 regardless of affinity.
    #[test]
    fn repair_bonus_zero_when_no_stored_goal() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let (world, actor, snap) = make_world_and_actor(&content, &difficulty, pos);
        let maps = empty_maps();
        let reservations = Reservations::default();

        // ── 2. Context (last_goal = None) ──
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        assert!(scoring.last_goal.is_none());
        let mut rng = combat_engine::DiceRng::default();
        let stage = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        // ── 3. ModifierCtx ──
        let actor_view = snap.unit(actor.entity).unwrap();
        let actor_value = unit_value(actor_view, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let ctx = ModifierCtx {
            stage: &stage,
            summon_dpr: &summon_dpr,
            actor_value,
            repair_weights,
        };

        // ── 4. Act ──
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
        let result = MODIFIER.modify(&plan, &ann, &ctx);

        // ── 5. Assert ──
        assert_eq!(result, 0.0);
    }

    fn make_modifier_ctx<'w, 's, 'a>(
        stage: &'a StageCtx<'w, 's>,
        actor: &crate::combat::ai::world::snapshot::UnitSnapshot,
        snap: &'s crate::combat::ai::world::snapshot::BattleSnapshot,
        world: &'w AiWorld<'w>,
        summon_dpr: &'a HashMap<String, f32>,
    ) -> ModifierCtx<'w, 's, 'a> {
        let actor_view = snap.unit(actor.entity).unwrap();
        let actor_value = unit_value(actor_view, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        ModifierCtx {
            stage,
            summon_dpr,
            actor_value,
            repair_weights,
        }
    }

    /// Higher continue_commitment → larger repair bonus.
    /// bonus(c=0) = agg × 1.0 × scale; bonus(c=1) = agg × 2.0 × scale → diff = agg × scale.
    #[test]
    fn repair_bonus_modulated_by_continue_commitment() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let (world, actor, snap) = make_world_and_actor(&content, &difficulty, pos);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let target_entity = bevy::prelude::Entity::from_raw_u32(42).unwrap();
        let stored_goal = make_stored_goal(target_entity, pos);
        let affinity = RepairAffinity {
            goal_alignment: 1.0,
            region_alignment: 0.0,
            method_alignment: 0.0,
            severity_factor: 1.0,
            ttl_factor: 1.0,
            confidence: 1.0,
        };
        let summon_dpr = HashMap::new();

        // ── 2+3. Context / ModifierCtx: two variants with different commitment ──
        let bonus_no_commitment = {
            let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            scoring.last_goal = Some(&stored_goal);
            scoring.need_signals = NeedSignals {
                continue_commitment: 0.0,
                ..Default::default()
            };
            let mut rng = combat_engine::DiceRng::default();
            let stage = StageCtx::new(
                &scoring,
                TacticalIntent::Reposition,
                IntentReason::NoRuleDefault,
                pos,
                &mut rng,
            );
            let ctx = make_modifier_ctx(&stage, &actor, &snap, &world, &summon_dpr);
            let mut plan = inert_plan(pos);
            plan.annotation.repair_affinity = affinity;
            let ann = plan.annotation.clone();
            // ── 4. Act ──
            MODIFIER.modify(&plan, &ann, &ctx)
        };

        let bonus_full_commitment = {
            let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            scoring.last_goal = Some(&stored_goal);
            scoring.need_signals = NeedSignals {
                continue_commitment: 1.0,
                ..Default::default()
            };
            let mut rng = combat_engine::DiceRng::default();
            let stage = StageCtx::new(
                &scoring,
                TacticalIntent::Reposition,
                IntentReason::NoRuleDefault,
                pos,
                &mut rng,
            );
            let ctx = make_modifier_ctx(&stage, &actor, &snap, &world, &summon_dpr);
            let mut plan = inert_plan(pos);
            plan.annotation.repair_affinity = affinity;
            let ann = plan.annotation.clone();
            // ── 4. Act ──
            MODIFIER.modify(&plan, &ann, &ctx)
        };

        // ── 5. Assert ──
        assert!(
            bonus_full_commitment > bonus_no_commitment,
            "full continue_commitment must yield higher repair bonus: no={bonus_no_commitment} full={bonus_full_commitment}"
        );
        let repair_weights = actor.role.repair_weights(world.tuning);
        let agg = affinity.aggregate(&repair_weights).max(0.0);
        let expected_diff = agg * world.tuning.thresholds.repair_bonus_scale;
        let actual_diff = bonus_full_commitment - bonus_no_commitment;
        assert!(
            (actual_diff - expected_diff).abs() < 1e-5,
            "bonus diff should equal aggregate × scale = {expected_diff}, got {actual_diff}"
        );
    }

    /// scale=0 → zero bonus; scale=0.4, commitment=0.4 → bonus = agg × 1.4 × 0.4.
    #[test]
    fn repair_bonus_scaled_by_threshold() {
        use crate::combat::ai::config::tuning::AiTuning;

        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let target_entity = bevy::prelude::Entity::from_raw_u32(42).unwrap();
        let stored_goal = make_stored_goal(target_entity, pos);
        let affinity = RepairAffinity {
            goal_alignment: 1.0,
            region_alignment: 0.0,
            method_alignment: 0.0,
            severity_factor: 1.0,
            ttl_factor: 1.0,
            confidence: 1.0,
        };
        let summon_dpr = HashMap::new();

        // ── 2. Context: Case A — scale = 0 ──
        let bonus_scale_zero = {
            let mut tuning = AiTuning::default();
            tuning.thresholds.repair_bonus_scale = 0.0;
            let world = AiWorld {
                content: &content,
                difficulty: &difficulty,
                tuning: &tuning,
                crit_fail_chance: 0.0,
                ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(),
                status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            };
            let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            scoring.last_goal = Some(&stored_goal);
            scoring.need_signals = NeedSignals {
                continue_commitment: 0.4,
                ..Default::default()
            };
            let mut rng = combat_engine::DiceRng::default();
            let stage = StageCtx::new(
                &scoring,
                TacticalIntent::Reposition,
                IntentReason::NoRuleDefault,
                pos,
                &mut rng,
            );
            let ctx = make_modifier_ctx(&stage, &actor, &snap, &world, &summon_dpr);
            let mut plan = inert_plan(pos);
            plan.annotation.repair_affinity = affinity;
            let ann = plan.annotation.clone();
            // ── 4. Act ──
            MODIFIER.modify(&plan, &ann, &ctx)
        };

        // ── 2. Context: Case B — scale = 0.4 ──
        let bonus_scale_04 = {
            let mut tuning = AiTuning::default();
            tuning.thresholds.repair_bonus_scale = 0.4;
            let world = AiWorld {
                content: &content,
                difficulty: &difficulty,
                tuning: &tuning,
                crit_fail_chance: 0.0,
                ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(),
                status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            };
            let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            scoring.last_goal = Some(&stored_goal);
            scoring.need_signals = NeedSignals {
                continue_commitment: 0.4,
                ..Default::default()
            };
            let mut rng = combat_engine::DiceRng::default();
            let stage = StageCtx::new(
                &scoring,
                TacticalIntent::Reposition,
                IntentReason::NoRuleDefault,
                pos,
                &mut rng,
            );
            let ctx = make_modifier_ctx(&stage, &actor, &snap, &world, &summon_dpr);
            let mut plan = inert_plan(pos);
            plan.annotation.repair_affinity = affinity;
            let ann = plan.annotation.clone();
            // ── 4. Act ──
            MODIFIER.modify(&plan, &ann, &ctx)
        };

        // ── 5. Assert ──
        assert_eq!(bonus_scale_zero, 0.0, "scale=0 must yield zero bonus");

        let repair_weights = actor.role.repair_weights(&AiTuning::default());
        let agg = affinity.aggregate(&repair_weights).max(0.0);
        let expected_bonus = agg * (1.0 + 0.4) * 0.4;
        assert!(
            (bonus_scale_04 - expected_bonus).abs() < 1e-5,
            "bonus with scale=0.4, commitment=0.4 should be {expected_bonus}, got {bonus_scale_04}"
        );
    }

    /// Pin: bonus = aggregate.max(0) × (1 + continue_commitment) × scale.
    ///
    /// Use goal_alignment=1, rest=0 → combined = 1×goal_w.
    /// Multiply by severity=1, ttl=1, confidence=1 → aggregate = goal_w.
    /// With continue_commitment=0.5, scale=0.4:
    ///   expected = goal_w × 1.0 × (1.0 + 0.5) × 0.4 = goal_w × 0.6
    #[test]
    fn repair_bonus_matches_legacy_formula() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let (world, actor, snap) = make_world_and_actor(&content, &difficulty, pos);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let target_entity = bevy::prelude::Entity::from_raw_u32(42).unwrap();
        let stored_goal = make_stored_goal(target_entity, pos);
        let affinity = RepairAffinity {
            goal_alignment: 1.0,
            region_alignment: 0.0,
            method_alignment: 0.0,
            severity_factor: 1.0,
            ttl_factor: 1.0,
            confidence: 1.0,
        };

        // ── 2. Context ──
        let mut scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        scoring.last_goal = Some(&stored_goal);
        scoring.need_signals = NeedSignals {
            continue_commitment: 0.5,
            ..Default::default()
        };
        let mut rng = combat_engine::DiceRng::default();
        let stage = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        // ── 3. ModifierCtx ──
        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let actor_view = snap.unit(actor.entity).unwrap();
        let actor_value = unit_value(actor_view, world.content);
        let ctx = ModifierCtx {
            stage: &stage,
            summon_dpr: &summon_dpr,
            actor_value,
            repair_weights,
        };

        // ── 4. Act ──
        let mut plan = inert_plan(pos);
        plan.annotation.repair_affinity = affinity;
        let ann = plan.annotation.clone();
        let got = MODIFIER.modify(&plan, &ann, &ctx);

        // ── 5. Assert ──
        let aggregate = affinity.aggregate(&repair_weights).max(0.0);
        let continue_commitment = 0.5_f32;
        let bonus_scale = world.tuning.thresholds.repair_bonus_scale;
        let expected = aggregate * (1.0 + continue_commitment) * bonus_scale;
        assert!(
            (got - expected).abs() < 1e-6,
            "repair_bonus formula mismatch: expected {expected}, got {got}"
        );
    }
}
