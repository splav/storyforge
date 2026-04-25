//! ViabilityStage — step 7.1.
//!
//! Replicates `PlanRanking::apply_viability` as a `PlanStage`. Reads the
//! current intent from `ctx`, checks whether any plan meets the intent's
//! signal threshold, and if not, swaps intent + rescores. Writes per-plan
//! `annotation.viability` with the gate result and adjusted score.

use crate::combat::ai::intent::{
    default_focus_target, intent_viability_threshold, IntentReason, TacticalIntent,
};
use crate::combat::ai::outcome::ViabilityResult;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::{rescore_with_intent};

pub struct ViabilityStage;

impl PlanStage for ViabilityStage {
    fn name(&self) -> &'static str {
        "viability"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(threshold) = intent_viability_threshold(&ctx.intent) else {
            // No threshold for this intent — all plans trivially pass.
            for ann in pool.annotations.iter_mut() {
                ann.viability = ViabilityResult { passed: true, adjusted_score: 0.0 };
            }
            return;
        };

        let max_align = pool
            .raw_factors
            .iter()
            .map(|f| f.intent)
            .fold(f32::NEG_INFINITY, f32::max);

        if max_align >= threshold {
            // Gate passes — record current scores as adjusted scores.
            for (ann, &score) in pool.annotations.iter_mut().zip(pool.scored.iter()) {
                ann.viability = ViabilityResult { passed: true, adjusted_score: score };
            }
            return;
        }

        // Gate fails — determine fallback intent.
        let scoring = ctx.scoring;
        let hp_pct = scoring.active.hp_pct();
        let actor_danger = scoring.maps.danger.get(scoring.active.pos);
        let midpanic_hp = scoring.world.difficulty.midpanic_hp_threshold();
        let panic_danger = scoring.world.difficulty.awareness_danger_threshold(scoring.world.tuning);
        let midpanic = hp_pct < midpanic_hp && actor_danger > panic_danger;

        let candidate: Option<(TacticalIntent, IntentReason)> = if midpanic {
            Some((
                TacticalIntent::ProtectSelf,
                IntentReason::MidpanicFallback {
                    hp_pct,
                    midpanic_hp,
                    danger: actor_danger,
                    panic_danger,
                    max_align,
                    threshold,
                },
            ))
        } else {
            let exclude = match &ctx.intent {
                TacticalIntent::FocusTarget { target } => Some(*target),
                _ => None,
            };
            let from_kind = ctx.intent.kind();
            default_focus_target(scoring.active, scoring.snap, &pool.plans, ctx.actor_pos, exclude)
                .map(|t| {
                    (
                        TacticalIntent::FocusTarget { target: t },
                        IntentReason::ViabilityFallback {
                            from: from_kind,
                            max_align,
                            threshold,
                        },
                    )
                })
        };

        let swapped = if let Some((new_intent, new_reason)) = candidate {
            if ctx.intent.kind() != new_intent.kind()
                || ctx.intent.target() != new_intent.target()
            {
                ctx.intent = new_intent;
                ctx.intent_reason = new_reason;
                pool.scored = rescore_with_intent(
                    &mut pool.plans,
                    &mut pool.raw_factors,
                    &ctx.intent,
                    scoring,
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // Write per-plan viability result (passed=false means a swap occurred).
        for (ann, &score) in pool.annotations.iter_mut().zip(pool.scored.iter()) {
            ann.viability = ViabilityResult { passed: !swapped, adjusted_score: score };
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::PlanFactors;
    use crate::combat::ai::intent::IntentReason;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn pool_with_intent_factor(factor: f32) -> ScoredPool {
        let plan = TurnPlan::default();
        let mut pool = ScoredPool::new(vec![plan]);
        pool.raw_factors[0] = PlanFactors { intent: factor, ..PlanFactors::default() };
        pool.scored[0] = 0.5;
        pool
    }

    fn ctx_for_actor<'w, 's>(
        scoring: &'s crate::combat::ai::utility::ScoringCtx<'w, 's>,
        intent: TacticalIntent,
        reason: IntentReason,
        actor_pos: crate::game::hex::Hex,
        rng: &'s mut DiceRng,
    ) -> StageCtx<'w, 's> {
        StageCtx::new(scoring, intent, reason, actor_pos, rng)
    }

    // ── above threshold: no-op ─────────────────────────────────────────────

    #[test]
    fn viability_stage_above_threshold_is_noop() {
        // Reposition threshold is 0.01. intent_factor=0.5 >> threshold → passes.
        let mut pool = pool_with_intent_factor(0.5);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = ctx_for_actor(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );

        ViabilityStage.apply(&mut pool, &mut ctx);

        assert!(matches!(ctx.intent, TacticalIntent::Reposition));
        assert!(matches!(ctx.intent_reason, IntentReason::NoRuleDefault));
        assert!(pool.annotations[0].viability.passed);
        // adjusted_score equals the pre-viability score (0.5)
        assert_eq!(pool.annotations[0].viability.adjusted_score, 0.5);
    }

    // ── midpanic swap to ProtectSelf ──────────────────────────────────────

    #[test]
    fn viability_stage_switches_intent_on_midpanic() {
        // Low HP + high danger + zero intent alignment → midpanic branch.
        let mut pool = pool_with_intent_factor(0.0);
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(3).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(pos, 1.0);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = ctx_for_actor(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        ViabilityStage.apply(&mut pool, &mut ctx);

        assert!(matches!(ctx.intent, TacticalIntent::ProtectSelf), "expected ProtectSelf");
        assert!(
            matches!(ctx.intent_reason, IntentReason::MidpanicFallback { .. }),
            "expected MidpanicFallback, got {:?}", ctx.intent_reason,
        );
        // swap occurred → passed=false
        assert!(!pool.annotations[0].viability.passed);
    }

    // ── annotation is written ─────────────────────────────────────────────

    #[test]
    fn viability_stage_writes_annotation_section() {
        // Any call must populate pool.annotations[i].viability.
        let mut pool = pool_with_intent_factor(0.5);
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = ctx_for_actor(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );

        ViabilityStage.apply(&mut pool, &mut ctx);

        // viability field must be set (not the zero-default with adjusted_score=0.0
        // from the ScoredPool::new initializer — above-threshold path writes the
        // pre-viability score 0.5).
        assert!(pool.annotations[0].viability.passed);
        assert!(pool.annotations[0].viability.adjusted_score > 0.0);
    }

    // ── no enemies: intent kept ────────────────────────────────────────────

    #[test]
    fn viability_stage_no_enemies_keeps_intent() {
        // Zero intent alignment, no enemies to fall back to → no swap.
        let mut pool = pool_with_intent_factor(0.0);
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = ctx_for_actor(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );

        ViabilityStage.apply(&mut pool, &mut ctx);

        assert!(matches!(ctx.intent, TacticalIntent::Reposition));
        // no swap → passed=true
        assert!(pool.annotations[0].viability.passed);
    }

    fn empty_content() -> crate::content::content_view::ContentView {
        crate::combat::ai::test_helpers::empty_content()
    }
}
