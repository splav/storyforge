//! RareResourceForLowImpact critic — step 10.3.
//!
//! Fires when a plan contains a Cast step with a **damaging** ability that has
//! a high mana cost but low actual enemy damage relative to its expected
//! output. Spending a scarce resource for poor damage returns is a strategic
//! waste.
//!
//! **Scope: damage-abilities only.** Status-only casts (CC, buffs, debuffs)
//! are skipped — their value is in the status, not in damage, and a low-damage
//! ratio for a stun/silence is structurally normal. Wasted-buff cases are
//! handled by `BuffIntoVoid` in this same wave; dedicated status-value critics
//! belong in a future wave (master plan backlog).
//!
//! Fire condition:
//!   `def.effect.calc(actor.caster_ctx).expected() > 0` (ability deals damage)
//!   AND `mana_cost >= 30` AND `impact_ratio < 0.5`
//!   where `impact_ratio = outcome.enemy_damage / expected_damage`.
//!
//! Multiplier: monotone in `(0.5 - impact_ratio)`, floored at 0.5.
//!   `multiplier = (1.0 - 0.4 * (0.5 - ratio) / 0.5).max(0.5)`
//!   → 1.0 at ratio=0.5 (boundary, doesn't fire), 0.6 at ratio=0.0, floored at 0.5.

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::content::abilities::EffectCalcExt;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::core::ResourceKind;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Mana cost threshold above which the critic checks impact ratio.
const MANA_COST_THRESHOLD: i32 = 30;

/// Impact ratio below which the critic fires (actual / expected < 0.5).
const IMPACT_RATIO_THRESHOLD: f32 = 0.5;

/// Hard floor for the multiplier.
const MULTIPLIER_FLOOR: f32 = 0.5;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — all thresholds are module constants.
pub struct RareResourceForLowImpact;

impl PlanCritic for RareResourceForLowImpact {
    fn name(&self) -> &'static str {
        "rare_resource_for_low_impact"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        _ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        for (step_idx, step) in plan.steps.iter().enumerate() {
            let PlanStep::Cast { ability, .. } = step else {
                continue;
            };

            let Some(def) = ctx.world.content.abilities.get(ability) else {
                continue;
            };

            // Find mana cost.
            let mana_cost = def
                .costs
                .iter()
                .find(|c| c.resource == ResourceKind::Mana)
                .map_or(0, |c| c.amount);

            if mana_cost < MANA_COST_THRESHOLD {
                continue;
            }

            // Damage-only scope: skip status-only / utility abilities — their
            // value lies outside damage and a low impact_ratio there is
            // structural, not a waste.
            let Some(expected_damage) = def
                .effect
                .calc(&ctx.active.caster_ctx)
                .map(|ec| ec.expected())
                .filter(|&e| e > 0.0)
            else {
                continue;
            };

            // Actual enemy damage from the corresponding outcome, if available.
            // Outcomes live on `TurnPlan.annotation` (populated by generator);
            // pipeline annotation outcomes are dead during pipeline.
            let actual_damage = plan
                .annotation
                .outcomes
                .get(step_idx)
                .map_or(0.0, |o| o.enemy_damage);

            let impact_ratio = (actual_damage / expected_damage).min(1.0);

            if impact_ratio >= IMPACT_RATIO_THRESHOLD {
                continue;
            }

            // Monotone multiplier: 1.0 at threshold boundary → MULTIPLIER_FLOOR at ratio=0.
            let shortfall = IMPACT_RATIO_THRESHOLD - impact_ratio; // in (0, 0.5]
            let multiplier =
                (1.0 - 0.4 * shortfall / IMPACT_RATIO_THRESHOLD).max(MULTIPLIER_FLOOR);

            // Cast mana_cost to u8 (saturate at 255 for the reason field; in
            // practice all costs fit in u8 per content convention).
            let cost_u8 = mana_cost.min(u8::MAX as i32) as u8;

            return Some(CriticHit {
                critic: CriticKind::RareResourceForLowImpact,
                multiplier,
                reason: CriticReason::RareResourceForLowImpact {
                    ability: ability.to_string(),
                    cost: cost_u8,
                    impact_ratio,
                },
            });
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, PlanCritic};
    use crate::combat::ai::outcome::{ActionOutcomeEstimate, PlanAnnotation};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
        snapshot_from,
    };
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr, ResourceKind};
    use crate::content::abilities::ResourceCost;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    /// Build an expensive mana-cost spell with the given expected dice damage.
    fn expensive_spell(id: &str, mana_cost: i32, dice: DiceExpr) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::SpellDamage { dice },
                costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: mana_cost }],
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
            },
        }
    }

    fn cast_plan_with_outcome(
        ability: &str,
        target_entity: Entity,
        target_pos: crate::game::hex::Hex,
        enemy_damage: f32,
    ) -> (TurnPlan, PlanAnnotation) {
        let mut plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from(ability),
                target: target_entity,
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default()],
            ..TurnPlan::default()
        };
        plan.annotation.outcomes.push(ActionOutcomeEstimate {
            enemy_damage,
            ..Default::default()
        });
        let ann = PlanAnnotation::default();
        (plan, ann)
    }

    // ── fires on canonical case (high mana, negligible damage) ───────────────

    #[test]
    fn rare_resource_fires_on_canonical_case() {
        // ── 1. Test data ──
        // Spell: mana_cost=40, dice=4d6 (expected ~14). Actual enemy_damage=2.
        // impact_ratio = 2/14 ≈ 0.14 < 0.5 → critic must fire.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos)
            .caster_ctx(CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None })
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        let (plan, ann) = cast_plan_with_outcome("bolt", target_entity, target_pos, 2.0);

        // ── 2. Context ──
        let mut content = empty_content();
        let dice = DiceExpr::new(4, 6, 0); // Expected damage: 4d6 + 0 = ~14 expected.
        content.abilities.insert(AbilityId::from("bolt"), expensive_spell("bolt", 40, dice));
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        // ── 3. Pool (N/A — critic tested via evaluate()) ──
        // ── 4. Act ──
        let result = RareResourceForLowImpact.evaluate(&plan, &ann, &ctx);

        // ── 5. Assert ──
        assert!(result.is_some(), "critic must fire: high mana cost with negligible damage");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::RareResourceForLowImpact);
        assert!(hit.multiplier < 1.0, "multiplier must penalise, got {}", hit.multiplier);
        assert!(hit.multiplier >= MULTIPLIER_FLOOR, "multiplier must not go below floor {MULTIPLIER_FLOOR}");
        if let CriticReason::RareResourceForLowImpact { cost, impact_ratio, .. } = hit.reason {
            assert_eq!(cost, 40);
            assert!(impact_ratio < IMPACT_RATIO_THRESHOLD, "impact_ratio must be below threshold");
        } else {
            panic!("expected RareResourceForLowImpact reason, got {:?}", hit.reason);
        }
    }

    // ── passes on clean plan (cheap spell or good impact) ────────────────────

    #[test]
    fn rare_resource_passes_on_clean_plan() {
        // ── 1. Test data ──
        // Case 1: spell with mana_cost=10 (below threshold) — must not fire.
        // Case 2: expensive spell that actually deals close-to-expected damage — must not fire.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        // ── 2. Context ──
        let mut content = empty_content();
        let dice = DiceExpr::new(2, 6, 0); // expected ~7
        content.abilities.insert(AbilityId::from("cheap"), expensive_spell("cheap", 10, dice.clone()));
        content.abilities.insert(AbilityId::from("effective"), expensive_spell("effective", 40, dice));
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        // ── 3. Plans ──
        let (plan_cheap, ann_cheap) = cast_plan_with_outcome("cheap", target_entity, target_pos, 0.0);
        // Expensive but effective spell (actual damage well above threshold).
        let (plan_eff, ann_eff) = cast_plan_with_outcome("effective", target_entity, target_pos, 12.0);

        // ── 4+5. Act + Assert ──
        assert!(
            RareResourceForLowImpact.evaluate(&plan_cheap, &ann_cheap, &ctx).is_none(),
            "cheap spell must not fire",
        );
        assert!(
            RareResourceForLowImpact.evaluate(&plan_eff, &ann_eff, &ctx).is_none(),
            "expensive but effective spell must not fire",
        );
    }

    // ── multiplier scales monotonically with impact ratio ─────────────────────

    #[test]
    fn rare_resource_severity_scales_with_input() {
        // ── 1. Test data ──
        // Two plans: very low impact (ratio≈0.07) vs moderate low impact (ratio≈0.35).
        // Very-low must produce strictly lower multiplier than moderate-low.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        // ── 2. Context ──
        let mut content = empty_content();
        let dice = DiceExpr::new(4, 6, 0); // expected ~14
        content.abilities.insert(AbilityId::from("bolt"), expensive_spell("bolt", 40, dice));
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        // ── 3. Plans ──
        // Very low impact: actual_damage=1 (ratio ≈ 1/14 ≈ 0.07).
        let (plan_vl, ann_vl) = cast_plan_with_outcome("bolt", target_entity, target_pos, 1.0);
        // Moderate low impact: actual_damage=5 (ratio ≈ 5/14 ≈ 0.36).
        let (plan_ml, ann_ml) = cast_plan_with_outcome("bolt", target_entity, target_pos, 5.0);

        // ── 4. Act ──
        let hit_vl = RareResourceForLowImpact.evaluate(&plan_vl, &ann_vl, &ctx);
        let hit_ml = RareResourceForLowImpact.evaluate(&plan_ml, &ann_ml, &ctx);

        // ── 5. Assert ──
        assert!(hit_vl.is_some(), "very-low-impact case must fire");
        assert!(hit_ml.is_some(), "moderate-low-impact case must fire");

        let mult_vl = hit_vl.unwrap().multiplier;
        let mult_ml = hit_ml.unwrap().multiplier;
        assert!(
            mult_vl <= mult_ml,
            "very-low impact ({mult_vl}) must be at least as punishing as moderate-low ({mult_ml})",
        );
    }

    // ── status-only abilities are skipped, regardless of cost ────────────────

    #[test]
    fn rare_resource_skips_status_only_abilities() {
        // ── 1. Test data ──
        // Expensive status-only ability (cost=40, no damage). The critic must
        // not fire — value is in the status, not in damage.
        use crate::content::abilities::{StatusApplication, StatusOn};
        use crate::core::StatusId;

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        let (plan, ann) = cast_plan_with_outcome("hard_stun", target_entity, target_pos, 0.0);

        // ── 2. Context ──
        let mut content = empty_content();
        let mut stun = expensive_spell("hard_stun", 40, DiceExpr::new(0, 0, 0));
        stun.effect = EffectDef::None;
        stun.statuses = vec![StatusApplication {
            status: StatusId::from("stunned"),
            on: StatusOn::Target,
            duration_rounds: 2,
        }];
        content.abilities.insert(AbilityId::from("hard_stun"), stun);
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        // ── 3. Pool (N/A) ──
        // ── 4+5. Act + Assert ──
        assert!(
            RareResourceForLowImpact.evaluate(&plan, &ann, &ctx).is_none(),
            "expensive status-only ability must not fire — damage is not its value axis",
        );
    }
}
