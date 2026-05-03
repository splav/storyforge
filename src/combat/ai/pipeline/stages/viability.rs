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
use crate::combat::ai::scoring::factors::aggregate::rescore_with_intent;

pub struct ViabilityStage;

impl PlanStage for ViabilityStage {
    fn name(&self) -> &'static str {
        "viability"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(threshold) = intent_viability_threshold(&ctx.intent) else {
            // No threshold for this intent — all plans trivially pass.
            for ann in pool.annotations.iter_mut() {
                ann.viability = ViabilityResult { passed: true, adjusted_score: ann.score };
            }
            return;
        };

        let max_align = pool
            .annotations
            .iter()
            .map(|a| a.factors.get_plan(crate::combat::ai::scoring::factors::PlanFactor::Intent))
            .fold(f32::NEG_INFINITY, f32::max);

        if max_align >= threshold {
            // Gate passes — record current scores as adjusted scores.
            for ann in pool.annotations.iter_mut() {
                ann.viability = ViabilityResult { passed: true, adjusted_score: ann.score };
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
                // Extract raw_factors as a mut slice for rescore_with_intent.
                let mut raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
                let new_scores = rescore_with_intent(
                    &mut pool.plans,
                    &mut raw_factors,
                    &ctx.intent,
                    scoring,
                );
                for (ann, (new_score, new_raw)) in pool.annotations.iter_mut().zip(new_scores.into_iter().zip(raw_factors.into_iter())) {
                    ann.set_score(new_score);
                    ann.factors = new_raw;
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        // Write per-plan viability result (passed=false means a swap occurred).
        for ann in pool.annotations.iter_mut() {
            ann.viability = ViabilityResult { passed: !swapped, adjusted_score: ann.score };
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::intent::IntentReason;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn pool_with_intent_factor(factor: f32) -> crate::combat::ai::pipeline::ScoredPool {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::Intent, factor);
        PoolBuilder::new(vec![TurnPlan::default()])
            .scores(&[0.5])
            .factors(vec![f])
            .build()
    }

    // ── above threshold: no-op ─────────────────────────────────────────────

    #[test]
    fn viability_stage_above_threshold_is_noop() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        // Reposition threshold is 0.01. intent_factor=0.5 >> threshold → passes.
        let mut pool = pool_with_intent_factor(0.5);

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 4. Act — capture ctx post-state ──
        let (post_intent, post_reason) = h.run(|ctx| {
            ViabilityStage.apply(&mut pool, ctx);
            (ctx.intent.clone(), ctx.intent_reason.clone())
        });

        // ── 5. Assert ──
        assert!(matches!(post_intent, TacticalIntent::Reposition));
        assert!(matches!(post_reason, IntentReason::NoRuleDefault));
        assert!(pool.annotations[0].viability.passed);
        // adjusted_score equals the pre-viability score (0.5)
        assert_eq!(pool.annotations[0].viability.adjusted_score, 0.5);
    }

    // ── midpanic swap to ProtectSelf ──────────────────────────────────────

    #[test]
    fn viability_stage_switches_intent_on_midpanic() {
        // ── 1. Test data ──
        // Low HP + high danger + zero intent alignment → midpanic branch.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(3).max_hp(20).build();
        let mut pool = pool_with_intent_factor(0.0);

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.maps.danger.add(hex_from_offset(0, 0), 1.0);

        // ── 4. Act ──
        let (post_intent, post_reason) = h.run(|ctx| {
            ViabilityStage.apply(&mut pool, ctx);
            (ctx.intent.clone(), ctx.intent_reason.clone())
        });

        // ── 5. Assert ──
        assert!(matches!(post_intent, TacticalIntent::ProtectSelf), "expected ProtectSelf");
        assert!(
            matches!(post_reason, IntentReason::MidpanicFallback { .. }),
            "expected MidpanicFallback, got {:?}", post_reason,
        );
        // swap occurred → passed=false
        assert!(!pool.annotations[0].viability.passed);
    }

    // ── annotation is written ─────────────────────────────────────────────

    #[test]
    fn viability_stage_writes_annotation_section() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let mut pool = pool_with_intent_factor(0.5);

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 4. Act ──
        h.run(|ctx| ViabilityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        // viability field must be set (not the zero-default with adjusted_score=0.0
        // from the ScoredPool::new initializer — above-threshold path writes the
        // pre-viability score 0.5).
        assert!(pool.annotations[0].viability.passed);
        assert!(pool.annotations[0].viability.adjusted_score > 0.0);
    }

    // ── no enemies: intent kept ────────────────────────────────────────────

    #[test]
    fn viability_stage_no_enemies_keeps_intent() {
        // ── 1. Test data ──
        // Zero intent alignment, no enemies to fall back to → no swap.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let mut pool = pool_with_intent_factor(0.0);

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 4. Act ──
        let post_intent = h.run(|ctx| {
            ViabilityStage.apply(&mut pool, ctx);
            ctx.intent.clone()
        });

        // ── 5. Assert ──
        assert!(matches!(post_intent, TacticalIntent::Reposition));
        // no swap → passed=true
        assert!(pool.annotations[0].viability.passed);
    }
}
