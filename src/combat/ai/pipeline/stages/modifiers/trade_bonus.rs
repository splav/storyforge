//! Plan-level trade bonus modifier (step 8.B).
//!
//! Lifted from `scorer.rs::plan_trade_bonus` (lines 419–428).
//! Thin wrapper over `trade_delta` + `trade_score`. Logic is identical.

use super::{ModifierCtx, PlanModifier};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::trade::{trade_delta, trade_score};

pub struct TradeBonus;
pub static MODIFIER: TradeBonus = TradeBonus;

impl PlanModifier for TradeBonus {
    fn name(&self) -> &'static str {
        "trade_bonus"
    }

    fn modify(&self, plan: &TurnPlan, _ann: &PlanAnnotation, ctx: &ModifierCtx<'_, '_, '_>) -> f32 {
        let active = ctx.stage.scoring.active;
        let snap = ctx.stage.scoring.snap;
        let world = ctx.stage.scoring.world;
        let br = trade_delta(plan, active, snap, world.content);
        trade_score(&br, ctx.actor_value)
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
    use crate::combat::ai::plan::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::scoring::trade::unit_value;
    use crate::combat::ai::orchestration::AiWorld;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    /// A neutral plan (no kills, actor alive) yields zero trade bonus.
    ///
    /// Migrated from `scorer.rs::trade_bonus_zero_for_neutral_plan` (line 2052).
    #[test]
    fn trade_bonus_zero_for_neutral_plan() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .ap(2)
            .ability_names(&["melee_attack"])
            .build();
        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: actor.entity,
                target_pos: pos,
            }],
            final_pos: pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            ..TurnPlan::default()
        };

        // ── 2. Context (uses real content for melee_attack ability) ──
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let world = AiWorld {
            content: &content,
            difficulty: &difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(),
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
        };
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = crate::core::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        // ── 3. ModifierCtx ──
        let actor_view = snap.unit(actor.entity).unwrap();
        let actor_value = unit_value(actor_view, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &summon_dpr, actor_value, repair_weights };

        // ── 4. Act ──
        let ann = crate::combat::ai::outcome::PlanAnnotation::default();
        let result = MODIFIER.modify(&plan, &ann, &ctx);

        // ── 5. Assert ──
        assert_eq!(result, 0.0);
    }

    /// Pin formula: a kill-plan against a valuable victim yields positive bonus,
    /// and a kill against a more valuable victim yields higher bonus than a less
    /// valuable one. This matches the legacy `plan_trade_bonus` behavior.
    #[test]
    fn trade_bonus_matches_legacy_formula() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .ap(2)
            .ability_names(&["melee_attack"])
            .build();
        let support = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .role(crate::combat::ai::config::role::AxisProfile { support: 1.0, ..Default::default() })
            .threat(6.0)
            .build();
        let rat = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0))
            .threat(1.0)
            .build();

        // ── 2. Context ──
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let snap = snapshot_from(vec![actor.clone(), support.clone(), rat.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let world = AiWorld {
            content: &content,
            difficulty: &difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: crate::combat::ai::test_helpers::empty_ability_tag_cache(),
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
        };
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = crate::core::DiceRng::default();
        let stage = StageCtx::new(&scoring, TacticalIntent::Reposition, IntentReason::NoRuleDefault, pos, &mut rng);

        // ── 3. ModifierCtx ──
        let actor_view = snap.unit(actor.entity).unwrap();
        let actor_value = unit_value(actor_view, world.content);
        let repair_weights = actor.role.repair_weights(world.tuning);
        let summon_dpr = HashMap::new();
        let ctx = ModifierCtx { stage: &stage, summon_dpr: &summon_dpr, actor_value, repair_weights };

        // ── 4. Act ──
        let ann = crate::combat::ai::outcome::PlanAnnotation::default();
        let mk_kill_plan = |victim: &crate::combat::ai::world::snapshot::UnitSnapshot| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: victim.entity,
                target_pos: victim.pos,
            }],
            final_pos: pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome { killed: vec![victim.entity], ..Default::default() }],
            ..TurnPlan::default()
        };
        let b_support = MODIFIER.modify(&mk_kill_plan(&support), &ann, &ctx);
        let b_rat = MODIFIER.modify(&mk_kill_plan(&rat), &ann, &ctx);

        // ── 5. Assert ──
        assert!(b_support > 0.0, "kill-support bonus must be positive: {b_support}");
        assert!(b_rat > 0.0, "kill-rat bonus must be positive: {b_rat}");
        assert!(
            b_support > b_rat,
            "trade_bonus must rank support-kill > rat-kill: {b_support} vs {b_rat}",
        );
    }
}
