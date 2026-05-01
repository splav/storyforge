//! Additive post-normalisation bonus for Summon plans (step 8.B).
//!
//! Lifted from `scorer.rs::plan_summon_bonus` (lines 370–411).
//! Logic is byte-for-byte identical; no formula changes.

use super::{ModifierCtx, PlanModifier};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::content::abilities::EffectDef;

pub struct SummonBonus;
pub static MODIFIER: SummonBonus = SummonBonus;

impl PlanModifier for SummonBonus {
    fn name(&self) -> &'static str {
        "summon_bonus"
    }

    fn modify(&self, plan: &TurnPlan, _ann: &PlanAnnotation, ctx: &ModifierCtx<'_, '_, '_>) -> f32 {
        let active = ctx.stage.scoring.active;
        let snap = ctx.stage.scoring.snap;
        let world = ctx.stage.scoring.world;
        let summon_dpr = ctx.summon_dpr;

        // Only LIVE summons occupy a cap slot. Dead units stay in the snapshot
        // with hp=0 — counting them would make the AI think the cap is reached
        // when the spawn side would happily summon more.
        let mut count = snap
            .units
            .iter()
            .filter(|u| u.summoner == Some(active.entity) && u.is_alive())
            .count() as f32;

        // Global saturation: total live allies on the actor's team (excluding actor).
        let total_allies = snap
            .units
            .iter()
            .filter(|u| u.team == active.team && u.entity != active.entity && u.is_alive())
            .count() as f32;
        // Saturation_mult computed once before the loop (legacy line :392).
        let saturation_mult = 0.65_f32.powf(total_allies);

        let mut total = 0.0f32;
        for step in &plan.steps {
            let PlanStep::Cast { ability, .. } = step else { continue };
            let Some(def) = world.content.abilities.get(ability) else { continue };
            let EffectDef::Summon { template, max_active } = &def.effect else { continue };

            let cap = max_active.unwrap_or(3).max(1) as f32;
            let decay = (1.0 - (count / cap)).max(0.0);
            if decay <= 0.0 {
                continue;
            }

            let dpr = summon_dpr.get(template).copied().unwrap_or(0.0);
            total += dpr * decay * saturation_mult;
            count += 1.0;
        }
        total
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::stages::modifiers::ModifierCtx;
    use crate::combat::ai::pipeline::StageCtx;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, UnitBuilder};
    use crate::combat::ai::scoring::trade::unit_value;
    use crate::combat::ai::orchestration::AiWorld;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    fn inert_plan(pos: crate::game::hex::Hex) -> TurnPlan {
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

    /// Plans without any Summon step get zero bonus.
    #[test]
    fn summon_bonus_zero_for_no_summon_plan() {
        let content = crate::combat::ai::test_helpers::empty_content();
        let difficulty = DifficultyProfile::default();
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let world = AiWorld { content: &content, difficulty: &difficulty, tuning: &content.ai_tuning, crit_fail_chance: 0.0, ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(), status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache() };
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = crate::core::rng::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        let summon_dpr = HashMap::new();
        let actor_value = unit_value(&actor, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &summon_dpr, actor_value, repair_weights };

        let plan = inert_plan(pos);
        let ann = crate::combat::ai::outcome::PlanAnnotation::default();
        assert_eq!(MODIFIER.modify(&plan, &ann, &ctx), 0.0);
    }

    /// Pin formula: single Summon step with known DPR → contribution matches
    /// hand-computed `dpr × cap_decay × saturation_mult`.
    ///
    /// Setup: actor has 0 existing summons, 0 other allies.
    /// - count = 0 → decay = (1 - 0/cap) = 1.0
    /// - total_allies = 0 → saturation_mult = 0.65^0 = 1.0
    /// - injected dpr = 7.0
    /// - expected = 7.0 × 1.0 × 1.0 = 7.0
    ///
    /// Uses real content to find an actual Summon ability; skips gracefully if none exists.
    #[test]
    fn summon_bonus_matches_legacy_formula() {
        let real_content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let pos = hex_from_offset(0, 0);
        let maps = empty_maps();
        let reservations = Reservations::default();

        // Find a Summon ability in real content.
        let summon_ability = real_content.abilities.iter().find(|(_, def)| {
            matches!(def.effect, EffectDef::Summon { .. })
        });

        let Some((summon_name, summon_def)) = summon_ability else {
            // No Summon ability in real content — the zero-path test still covers guard logic.
            return;
        };

        let EffectDef::Summon { template, max_active } = &summon_def.effect else { unreachable!() };

        let world = AiWorld { content: &real_content, difficulty: &difficulty, tuning: &real_content.ai_tuning, crit_fail_chance: 0.0, ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(), status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache() };
        // Actor with no existing summons, no other allies.
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = crate::core::rng::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        let injected_dpr = 7.0_f32;
        let mut dpr_cache = HashMap::new();
        dpr_cache.insert(template.clone(), injected_dpr);

        let actor_value = unit_value(&actor, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &dpr_cache, actor_value, repair_weights };

        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: summon_name.clone(),
                target: actor.entity,
                target_pos: pos,
            }],
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![],
            annotation: Default::default(),
        };
        let ann = crate::combat::ai::outcome::PlanAnnotation::default();

        // count = 0 existing summons, total_allies = 0 (only actor in snap).
        let cap = max_active.unwrap_or(3).max(1) as f32;
        let decay = (1.0 - (0.0_f32 / cap)).max(0.0); // = 1.0
        let saturation_mult = 0.65_f32.powf(0.0);      // = 1.0
        let expected = injected_dpr * decay * saturation_mult; // = 7.0

        let got = MODIFIER.modify(&plan, &ann, &ctx);
        assert!(
            (got - expected).abs() < 1e-5,
            "summon_bonus formula mismatch: expected {expected}, got {got}"
        );
    }
}
