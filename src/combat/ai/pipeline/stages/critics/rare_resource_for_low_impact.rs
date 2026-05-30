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
use combat_engine::ResourceKind;

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
                .calc(&ctx.active.cache.caster_ctx)
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
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
    use crate::combat::ai::outcome::{ActionOutcomeEstimate, PlanAnnotation};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{
        UnitBuilder, CriticScenarioBuilder,
        assert_critic_passes, run_critic,
    };
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use combat_engine::{AbilityId, DiceExpr, ResourceKind};
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
                requires_los: false,
                passive: None,
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
        (plan, PlanAnnotation::default())
    }

    // ── name is stable ────────────────────────────────────────────────────────

    #[test]
    fn rare_resource_name_is_stable() {
        let critic = RareResourceForLowImpact;
        assert_eq!(critic.name(), "rare_resource_for_low_impact");
    }

    // ── fires on canonical case (high mana, negligible damage) ───────────────

    #[test]
    fn rare_resource_fires_on_canonical_case() {
        // Spell: mana_cost=40, dice=4d6 (expected ~14). Actual enemy_damage=2.
        // impact_ratio = 2/14 ≈ 0.14 < 0.5 → critic must fire.
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos)
            .caster_ctx(CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None })
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("bolt", expensive_spell("bolt", 40, DiceExpr::new(4, 6, 0)))
            .build();
        let (plan, ann) = cast_plan_with_outcome("bolt", target_entity, target_pos, 2.0);

        let hit = run_critic(&RareResourceForLowImpact, &plan, &ann, &scn)
            .expect("critic must fire: high mana cost with negligible damage");
        assert_eq!(hit.critic, CriticKind::RareResourceForLowImpact);
        assert!(hit.multiplier < 1.0, "multiplier must penalise, got {}", hit.multiplier);
        assert!(hit.multiplier >= MULTIPLIER_FLOOR, "multiplier must not go below floor {MULTIPLIER_FLOOR}");
        let CriticReason::RareResourceForLowImpact { cost, impact_ratio, .. } = hit.reason else {
            panic!("expected RareResourceForLowImpact reason, got {:?}", hit.reason);
        };
        assert_eq!(cost, 40);
        assert!(impact_ratio < IMPACT_RATIO_THRESHOLD, "impact_ratio must be below threshold");
    }

    // ── passes on clean plan (cheap spell or good impact) ────────────────────

    #[test]
    fn rare_resource_passes_on_clean_plan() {
        // Case 1: spell with mana_cost=10 (below threshold) — must not fire.
        // Case 2: expensive spell that deals close-to-expected damage — must not fire.
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let dice   = DiceExpr::new(2, 6, 0); // expected ~7

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("cheap",     expensive_spell("cheap",     10, dice))
            .with_ability("effective", expensive_spell("effective", 40, dice))
            .build();

        let (plan_cheap, ann_cheap) = cast_plan_with_outcome("cheap",     target_entity, target_pos, 0.0);
        let (plan_eff,   ann_eff)   = cast_plan_with_outcome("effective", target_entity, target_pos, 12.0);

        assert_critic_passes(&RareResourceForLowImpact, &plan_cheap, &ann_cheap, &scn);
        assert_critic_passes(&RareResourceForLowImpact, &plan_eff,   &ann_eff,   &scn);
    }

    // ── multiplier scales monotonically with impact ratio ─────────────────────

    #[test]
    fn rare_resource_severity_scales_with_input() {
        // Very low impact (ratio≈0.07) vs moderate low impact (ratio≈0.35).
        // Very-low must produce a strictly lower (or equal) multiplier.
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("bolt", expensive_spell("bolt", 40, DiceExpr::new(4, 6, 0))) // expected ~14
            .build();

        let (plan_vl, ann_vl) = cast_plan_with_outcome("bolt", target_entity, target_pos, 1.0); // ratio ≈ 0.07
        let (plan_ml, ann_ml) = cast_plan_with_outcome("bolt", target_entity, target_pos, 5.0); // ratio ≈ 0.36

        let mult_vl = run_critic(&RareResourceForLowImpact, &plan_vl, &ann_vl, &scn)
            .expect("very-low-impact case must fire").multiplier;
        let mult_ml = run_critic(&RareResourceForLowImpact, &plan_ml, &ann_ml, &scn)
            .expect("moderate-low-impact case must fire").multiplier;

        assert!(
            mult_vl <= mult_ml,
            "very-low impact ({mult_vl}) must be at least as punishing as moderate-low ({mult_ml})",
        );
    }

    // ── status-only abilities are skipped, regardless of cost ────────────────

    #[test]
    fn rare_resource_skips_status_only_abilities() {
        // Expensive status-only ability (cost=40, no damage). The critic must
        // not fire — value is in the status, not in damage.
        use crate::content::abilities::{StatusApplication, StatusOn};
        use combat_engine::StatusId;

        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();

        let mut stun = expensive_spell("hard_stun", 40, DiceExpr::new(0, 0, 0));
        stun.effect   = EffectDef::None;
        stun.statuses = vec![StatusApplication {
            status: StatusId::from("stunned"),
            on: StatusOn::Target,
            duration_rounds: 2,
        }];

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("hard_stun", stun)
            .build();
        let (plan, ann) = cast_plan_with_outcome("hard_stun", target_entity, target_pos, 0.0);

        assert_critic_passes(&RareResourceForLowImpact, &plan, &ann, &scn);
    }

    // ── mana cost boundary: exactly at threshold ──────────────────────────────

    /// MANA_COST_THRESHOLD = 30. The check is `mana_cost < 30`, so:
    ///   cost=29  → skipped (passes)
    ///   cost=30  → checked; with low impact, fires
    ///
    /// The mutant `< → <=` would skip cost=30, so the fire case kills it.
    /// The pass case (cost=29) ensures the lower boundary is respected.
    #[test]
    fn rare_resource_mana_cost_boundary() {
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        // Small expected damage so the impact ratio is low when damage_out = 0.
        let dice = DiceExpr::new(2, 6, 0); // expected ~7

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("below_threshold", expensive_spell("below_threshold", 29, dice))
            .with_ability("at_threshold",    expensive_spell("at_threshold",    30, dice))
            .build();

        // cost=29: below threshold, must not fire even with zero damage.
        let (plan_below, ann_below) = cast_plan_with_outcome(
            "below_threshold", target_entity, target_pos, 0.0,
        );
        assert_critic_passes(&RareResourceForLowImpact, &plan_below, &ann_below, &scn);

        // cost=30: at threshold, with impact_ratio≈0 (zero actual damage), must fire.
        let (plan_at, ann_at) = cast_plan_with_outcome(
            "at_threshold", target_entity, target_pos, 0.0,
        );
        run_critic(&RareResourceForLowImpact, &plan_at, &ann_at, &scn)
            .expect("critic must fire when mana_cost == MANA_COST_THRESHOLD and impact is low");
    }

    // ── impact ratio boundary: exactly at 0.5 ────────────────────────────────

    /// IMPACT_RATIO_THRESHOLD = 0.5. The check is `impact_ratio >= 0.5`,
    /// meaning at exactly 0.5 the plan passes (skipped). Just below fires.
    ///
    /// The mutant `>= → >` would fire at ratio=0.5, killing this pass case.
    /// The fire case at ratio<0.5 confirms the normal branch is still reached.
    ///
    /// We use a spell with a known fixed expected damage. DiceExpr::new(0,0,20)
    /// has expected value = 20.0 (constant). Actual damage 10 → ratio = 0.5 exactly.
    #[test]
    fn rare_resource_impact_ratio_boundary() {
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        // Constant-damage spell: expected = 20 (0d0+20). Mana cost = 40 to exceed threshold.
        let const_spell = expensive_spell("const_dmg", 40, DiceExpr::new(0, 0, 20));

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("const_dmg", const_spell)
            .build();

        // actual = 10, expected = 20 → ratio = 10/20 = 0.5 exactly → must NOT fire.
        let (plan_at, ann_at) = cast_plan_with_outcome("const_dmg", target_entity, target_pos, 10.0);
        assert_critic_passes(&RareResourceForLowImpact, &plan_at, &ann_at, &scn);

        // actual = 9.9, expected = 20 → ratio = 0.495 < 0.5 → must fire.
        let (plan_below, ann_below) = cast_plan_with_outcome("const_dmg", target_entity, target_pos, 9.9);
        run_critic(&RareResourceForLowImpact, &plan_below, &ann_below, &scn)
            .expect("critic must fire when impact_ratio < IMPACT_RATIO_THRESHOLD");
    }

    // ── multiplier formula: tight range (catches arithmetic mutations) ────────

    /// With impact_ratio = 0.1 (actual=2 / expected=20) and default constants:
    ///   shortfall  = 0.5 - 0.1 = 0.4
    ///   multiplier = (1.0 - 0.4 * 0.4 / 0.5).max(0.5)
    ///              = (1.0 - 0.32).max(0.5)
    ///              = 0.68
    ///
    /// Mutations and their outputs (all outside [0.60, 0.76]):
    ///   shortfall `-→/` (line 105): shortfall = 0.5 / 0.1 = 5.0 → mult = max(1-3.2, 0.5) = 0.5
    ///   divisor `/→%` (line 107):   0.4 * 0.4 % 0.5 = 0.16 % 0.5 = 0.16 → mult = 0.84 (outside range)
    ///   divisor `/→*` (line 107):   0.4 * 0.4 * 0.5 = 0.08 → mult = 0.92 (outside range)
    ///   factor  `*→/` (line 107):   0.4 / 0.4 / 0.5 = 5.0 → mult = max(1-5, 0.5) = 0.5
    #[test]
    fn rare_resource_multiplier_tight_range() {
        let caster_pos    = hex_from_offset(0, 0);
        let target_pos    = hex_from_offset(3, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        // Constant-damage spell: expected = 20, mana_cost = 40.
        let const_spell = expensive_spell("const_dmg", 40, DiceExpr::new(0, 0, 20));

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("const_dmg", const_spell)
            .build();

        // actual = 2, expected = 20 → ratio = 0.1; expected multiplier ≈ 0.68.
        let (plan, ann) = cast_plan_with_outcome("const_dmg", target_entity, target_pos, 2.0);
        let hit = run_critic(&RareResourceForLowImpact, &plan, &ann, &scn)
            .expect("critic must fire");

        assert!(
            hit.multiplier > 0.60 && hit.multiplier < 0.76,
            "multiplier expected ≈ 0.68 (ratio=0.1), got {}",
            hit.multiplier,
        );
    }
}
