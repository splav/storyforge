//! HealWithoutRescueValue critic — step 10.3.
//!
//! Fires when a plan contains a heal cast aimed at an ally who does not need
//! healing — the target has high HP and is not in a dangerous position.
//! Healing a healthy unit out of danger is a pure resource waste.
//!
//! Fire condition:
//!   A `PlanStep::Cast` where:
//!   - The ability has a `Heal` effect OR `outcome.hp_restored > 0`.
//!   - The target ally has `hp_pct() > 0.7`.
//!   - The danger map value at the target's position is `< 0.3`.
//!
//! Multiplier: **0.4** (fixed — healing a healthy safe unit is a significant waste).

use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::utility::ScoringCtx;
use crate::content::abilities::EffectDef;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Score multiplier when healing a healthy, safe ally.
const HEAL_WITHOUT_RESCUE_MULTIPLIER: f32 = 0.4;

/// Target HP% above which the ally is considered "not in need of healing".
const HP_PCT_SAFE_THRESHOLD: f32 = 0.7;

/// Danger map value below which the target's position is considered safe.
const DANGER_SAFE_THRESHOLD: f32 = 0.3;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — thresholds are baked as module constants.
pub struct HealWithoutRescueValue;

impl PlanCritic for HealWithoutRescueValue {
    fn name(&self) -> &'static str {
        "heal_without_rescue_value"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        for (step_idx, step) in plan.steps.iter().enumerate() {
            let PlanStep::Cast { ability, target_pos, .. } = step else {
                continue;
            };

            let Some(def) = ctx.world.content.abilities.get(ability) else {
                continue;
            };

            // Determine whether this is a heal cast:
            // - effect is EffectDef::Heal, OR
            // - outcome records hp_restored > 0 (covers RestoreResources etc.)
            let is_heal_by_effect = matches!(def.effect, EffectDef::Heal { .. });
            let is_heal_by_outcome = ann
                .outcomes
                .get(step_idx)
                .is_some_and(|o| o.hp_restored > 0.0);

            if !is_heal_by_effect && !is_heal_by_outcome {
                continue;
            }

            // Look up target ally in snapshot.
            let Some(target) = ctx.snap.unit_at(*target_pos) else {
                continue;
            };

            let target_hp_pct = target.hp_pct();
            let target_danger = ctx.maps.danger.get(target.pos);

            if target_hp_pct > HP_PCT_SAFE_THRESHOLD && target_danger < DANGER_SAFE_THRESHOLD {
                return Some(CriticHit {
                    critic: CriticKind::HealWithoutRescueValue,
                    multiplier: HEAL_WITHOUT_RESCUE_MULTIPLIER,
                    reason: CriticReason::HealWithoutRescueValue {
                        target_hp_pct,
                        target_danger,
                    },
                });
            }
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::critics::{CriticKind, PlanCritic};
    use crate::combat::ai::outcome::{ActionOutcomeEstimate, PlanAnnotation};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
    use crate::core::{AbilityId, DiceExpr};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    fn heal_ability(id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::Heal { dice: DiceExpr::new(2, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn cast_heal_plan(
        ability: &str,
        target_entity: Entity,
        target_pos: crate::game::hex::Hex,
        caster_pos: crate::game::hex::Hex,
        hp_restored: f32,
    ) -> (TurnPlan, PlanAnnotation) {
        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from(ability),
                target: target_entity,
                target_pos,
            }],
            final_pos: caster_pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let mut ann = PlanAnnotation::default();
        ann.outcomes.push(ActionOutcomeEstimate {
            hp_restored,
            ..Default::default()
        });
        (plan, ann)
    }

    // ── fires on canonical case (full HP, low danger) ─────────────────────────

    #[test]
    fn heal_without_rescue_fires_on_canonical_case() {
        // Target: hp=28/30 (93%), danger=0.1 → both thresholds exceeded → fires.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(28)
            .max_hp(30)
            .build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));

        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![caster.clone(), target], 1);
        let maps = empty_maps(); // danger = 0.0 everywhere (< 0.3)
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        let target_entity = Entity::from_raw_u32(2).expect("valid");
        let (plan, ann) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 3.0);

        let result = HealWithoutRescueValue.evaluate(&plan, &ann, &ctx);
        assert!(result.is_some(), "critic must fire: healing healthy ally in safe position");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::HealWithoutRescueValue);
        assert!(
            (hit.multiplier - HEAL_WITHOUT_RESCUE_MULTIPLIER).abs() < 1e-6,
            "multiplier must be {HEAL_WITHOUT_RESCUE_MULTIPLIER}, got {}", hit.multiplier,
        );
        if let CriticReason::HealWithoutRescueValue { target_hp_pct, target_danger } = hit.reason {
            assert!(target_hp_pct > HP_PCT_SAFE_THRESHOLD);
            assert!(target_danger < DANGER_SAFE_THRESHOLD);
        } else {
            panic!("expected HealWithoutRescueValue reason, got {:?}", hit.reason);
        }
    }

    // ── passes on clean plan (target is low HP or in danger) ─────────────────

    #[test]
    fn heal_without_rescue_passes_on_clean_plan() {
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        // Case 1: target is low HP (50%) even though danger is 0.
        let low_hp_target = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(15)
            .max_hp(30)
            .build();
        let snap_lhp = BattleSnapshot::new(vec![caster.clone(), low_hp_target], 1);
        let maps_lhp = empty_maps();
        let ctx_lhp = make_scoring_ctx(&world, &snap_lhp, &maps_lhp, &reservations, &caster);
        let (plan_lhp, ann_lhp) =
            cast_heal_plan("heal", target_entity, target_pos, caster_pos, 10.0);
        assert!(
            HealWithoutRescueValue.evaluate(&plan_lhp, &ann_lhp, &ctx_lhp).is_none(),
            "must not fire: target hp_pct=50% is below safe threshold",
        );

        // Case 2: target has high HP (90%) but is in high danger (0.8).
        let healthy_target = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(27)
            .max_hp(30)
            .build();
        let snap_hd = BattleSnapshot::new(vec![caster.clone(), healthy_target], 1);
        let mut maps_hd = empty_maps();
        maps_hd.danger.add(target_pos, 0.8);
        let ctx_hd = make_scoring_ctx(&world, &snap_hd, &maps_hd, &reservations, &caster);
        let (plan_hd, ann_hd) =
            cast_heal_plan("heal", target_entity, target_pos, caster_pos, 3.0);
        assert!(
            HealWithoutRescueValue.evaluate(&plan_hd, &ann_hd, &ctx_hd).is_none(),
            "must not fire: target is in danger (0.8 >= 0.3 threshold)",
        );
    }

    // ── boundary: multiplier is fixed regardless of hp_pct magnitude ─────────

    #[test]
    fn heal_without_rescue_severity_scales_with_input() {
        // Both "nearly full HP + safe" and "completely full HP + safe" fire with
        // the same fixed multiplier. We verify that the boundary between firing
        // (hp_pct=0.75, danger=0.1) and not-firing (hp_pct=0.65, danger=0.1)
        // is exactly at hp_pct=0.7.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        // hp_pct = 0.75 (22.5/30 — round to 22/30 ≈ 0.733), danger=0.1 → fires.
        let target_fires = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(23)
            .max_hp(30) // 23/30 ≈ 0.767 > 0.7
            .build();
        let snap_f = BattleSnapshot::new(vec![caster.clone(), target_fires], 1);
        let maps_f = empty_maps();
        let ctx_f = make_scoring_ctx(&world, &snap_f, &maps_f, &reservations, &caster);
        let (plan_f, ann_f) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 3.0);
        let hit = HealWithoutRescueValue.evaluate(&plan_f, &ann_f, &ctx_f);
        assert!(hit.is_some(), "hp_pct≈0.77 (> 0.7) must fire");
        assert!(
            (hit.unwrap().multiplier - HEAL_WITHOUT_RESCUE_MULTIPLIER).abs() < 1e-6,
            "multiplier must be fixed at {HEAL_WITHOUT_RESCUE_MULTIPLIER}",
        );

        // hp_pct = 0.60 (18/30) → does not fire.
        let target_passes = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(18)
            .max_hp(30) // 18/30 = 0.60 < 0.7
            .build();
        let snap_p = BattleSnapshot::new(vec![caster.clone(), target_passes], 1);
        let maps_p = empty_maps();
        let ctx_p = make_scoring_ctx(&world, &snap_p, &maps_p, &reservations, &caster);
        let (plan_p, ann_p) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 8.0);
        assert!(
            HealWithoutRescueValue.evaluate(&plan_p, &ann_p, &ctx_p).is_none(),
            "hp_pct=0.60 (< 0.7) must not fire",
        );
    }
}
