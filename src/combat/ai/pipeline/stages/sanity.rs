//! SanityStage — step 7.1.
//!
//! Replicates `PlanRanking::apply_sanity` as a `PlanStage`. Applies
//! multiplicative sanity penalties/bonuses to `pool.scored` and writes the
//! per-plan hit breakdown into `pool.annotations[i].sanity`.

use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::sanity_adjust_plans;

pub struct SanityStage;

impl PlanStage for SanityStage {
    fn name(&self) -> &'static str {
        "sanity"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind};

        // Snapshot entry scores BEFORE sanity_adjust_plans mutates them —
        // needed to detect masked plans (NEG_INFINITY) for the invariant guard.
        let entry_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        let mut scores: Vec<f32> = entry_scores.clone();
        let breakdown = sanity_adjust_plans(&mut scores, &pool.plans, ctx.scoring);

        for (i, (ann, (new_score, hits))) in pool
            .annotations
            .iter_mut()
            .zip(scores.into_iter().zip(breakdown.into_iter()))
            .enumerate()
        {
            ann.score = new_score;

            // P3a.6: bridging-reset removed. FinalizeStage (upstream) sets
            // trace.base; this stage pushes multiplier hits on top of the
            // accumulated trace.
            for hit in &hits {
                ann.score_trace.push_multiplier(MultiplierHit {
                    kind: MultiplierKind::Sanity,
                    value: hit.multiplier,
                });
            }
            ann.sanity = hits;

            // Invariant: ann.score == trace.compute() for finite plans.
            // Skip masked plans (entry_score = NEG_INFINITY) — sanity_adjust_plans
            // leaves them unmutated, and NEG_INFINITY corner cases can produce NaN.
            if entry_scores[i].is_finite() {
                debug_assert!(
                    (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                    "P3a.6 invariant violated: plan[{i}] ann.score={} vs compute()={}",
                    ann.score,
                    ann.score_trace.compute(),
                );
            }
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
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use crate::core::DiceRng;

    fn make_move_plan(path: Vec<Hex>) -> TurnPlan {
        let final_pos = path.last().copied().unwrap_or_else(|| hex_from_offset(0, 0));
        TurnPlan {
            steps: vec![PlanStep::Move { path }],
            final_pos,
            ..TurnPlan::default()
        }
    }

    fn apply_sanity_to_two_plans(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        actor_hp: i32,
        actor_max_hp: i32,
        danger_on_final: Option<(Hex, f32)>,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(actor_hp)
            .max_hp(actor_max_hp)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        if let Some((tile, val)) = danger_on_final {
            maps.danger.add(tile, val);
        }
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip(scores.into_iter()) {
            ann.score = score;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            // In production, FinalizeStage sets this; unit tests call the stage directly.
            ann.score_trace.base = score;
        }
        SanityStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── no hits on a clean plan ────────────────────────────────────────────

    #[test]
    fn sanity_stage_no_hits_leaves_annotation_empty() {
        // Two plans, full HP, no danger — no sanity rule fires.
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let pool = apply_sanity_to_two_plans(plans, vec![0.5, 0.4], 20, 20, None);

        for ann in &pool.annotations {
            assert!(
                ann.sanity.is_empty(),
                "expected no sanity hits for healthy actor in safe tile, got {:?}",
                ann.sanity,
            );
        }
    }

    // ── residual-only: low-HP actor on danger tile must not produce any hits ──
    // (Survival was migrated to critics in 10.1; SanityRule no longer has
    //  that variant after 10.4 cleanup.)

    #[test]
    fn sanity_stage_no_hits_for_low_hp_danger_tile() {
        // Low-HP actor on a danger tile: before step 10.1 this triggered
        // the Survival sanity rule. After 10.4, the enum no longer has
        // Survival — this test pins that the annotation stays empty.
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        // 2/20 HP = 10%; danger tile on destination of plan 0.
        let pool = apply_sanity_to_two_plans(
            plans, vec![0.5, 0.4], 2, 20, Some((dest_a, 1.0)),
        );

        // No sanity rule should fire — the only active rules are
        // HealerExposure, RetreatTrap, SynergyBonus, none of which
        // trigger in this solo-actor scenario.
        for ann in &pool.annotations {
            assert!(
                ann.sanity.is_empty(),
                "no sanity hits expected for low-HP actor in danger tile (Survival is now a critic), got {:?}",
                ann.sanity,
            );
        }
    }

    fn empty_content() -> crate::content::content_view::ContentView {
        crate::combat::ai::test_helpers::empty_content()
    }

    // ── sanity_survives_adaptation_path (B3 regression) ──────────────────
    //
    // Regression test for B3 fix (step 11.0): in the old pipeline order
    // SanityStage ran before FinalizeStage, which would rescore ann.score
    // from raw factors — wiping the Sanity multipliers. In the new order:
    //   ModeSelection → Finalize → Sanity → Critics → ...
    // Sanity runs AFTER Finalize, so its adjustments survive.
    //
    // This test runs: FinalizeStage (pre-populated adaptation) → SanityStage,
    // and verifies that the output of Sanity is a *modified version* of the
    // Finalize output (not the pre-finalize score).

    #[test]
    fn sanity_survives_adaptation_path() {
        use crate::combat::ai::factors::PlanFactorValues;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::planning::types::TurnPlan;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        // Two plans: one with LastStand adaptation injected, one Default.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let mut pool = ScoredPool::new(plans);
        let pre_scores = [0.8_f32, 0.6_f32];
        for (ann, (&score, adaptation)) in pool.annotations.iter_mut().zip(
            pre_scores.iter().zip([
                Some(AdaptationData {
                    reason: AdaptationReason::ProtectSelfNoDefensive,
                    original_score: 0.8,
                }),
                None,
            ])
        ) {
            ann.score = score;
            ann.factors = PlanFactorValues::default();
            ann.adaptation = adaptation;
        }

        // Run Finalize then Sanity (mirroring new pipeline order).
        FinalizeStage.apply(&mut pool, &mut ctx);
        let scores_after_finalize: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        SanityStage.apply(&mut pool, &mut ctx);

        // SanityStage either leaves scores unchanged (no rule fired) or
        // applies multipliers. Either way: scores must be ≤ finalized score
        // (sanity is non-additive, only penalty/bonus ≤1 or mild bonus).
        // The key invariant: scores after Sanity are based on finalized scores,
        // not on the raw pre_scores — meaning Finalize + Sanity compose correctly.
        for (i, ann) in pool.annotations.iter().enumerate() {
            // Sanity either preserves or reduces; cannot exceed finalized score
            // by more than a small bonus (SynergyBonus is +5% max).
            let finalized = scores_after_finalize[i];
            assert!(
                ann.score <= finalized * 1.1 + 1e-5,
                "plan[{i}]: sanity score {score} unexpectedly far above finalized {finalized}",
                score = ann.score,
                finalized = finalized,
            );
        }
    }

    // ── P3a.3 — ScoreTrace integration tests ─────────────────────────────────

    /// Clean plan (full HP, no danger, 2-plan pool): no rules fire.
    /// After apply: trace.base == entry_score, multipliers.len() == 0,
    /// trace.compute() == entry_score.
    #[test]
    fn p3a_sanity_no_hits_trace_has_only_base() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let entry_scores = vec![0.7_f32, 0.5_f32];
        let pool = apply_sanity_to_two_plans(plans, entry_scores.clone(), 20, 20, None);

        for (i, ann) in pool.annotations.iter().enumerate() {
            let entry = entry_scores[i];
            assert!(
                (ann.score_trace.base - entry).abs() < 1e-6,
                "plan[{i}]: trace.base must equal entry_score={entry}, got {}",
                ann.score_trace.base,
            );
            assert!(
                ann.score_trace.multipliers.is_empty(),
                "plan[{i}]: multipliers must be empty when no sanity rules fire, got {:?}",
                ann.score_trace.multipliers,
            );
            assert!(
                (ann.score_trace.compute() - entry).abs() < 1e-6,
                "plan[{i}]: trace.compute() must equal entry_score={entry}, got {}",
                ann.score_trace.compute(),
            );
        }
    }

    /// Reuse sanity_survives_adaptation_path setup to verify per-plan invariants
    /// introduced by P3a.3:
    ///   - multipliers.len() == sanity.len()  (1-to-1 mapping)
    ///   - each multiplier has kind=Sanity and value == sanity[j].multiplier
    ///   - compute() == ann.score (with ε=1e-5)
    ///
    /// Triggers may or may not fire (depends on rules); the invariants hold in
    /// both cases: no-hit → both vecs empty, hit → both vecs non-empty.
    #[test]
    fn p3a_sanity_with_hits_pushes_multipliers() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::factors::PlanFactorValues;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::planning::types::TurnPlan;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let mut pool = ScoredPool::new(plans);
        for (ann, (&score, adaptation)) in pool.annotations.iter_mut().zip(
            [0.8_f32, 0.6_f32].iter().zip([
                Some(AdaptationData {
                    reason: AdaptationReason::ProtectSelfNoDefensive,
                    original_score: 0.8,
                }),
                None,
            ])
        ) {
            ann.score = score;
            ann.factors = PlanFactorValues::default();
            ann.adaptation = adaptation;
        }

        FinalizeStage.apply(&mut pool, &mut ctx);
        SanityStage.apply(&mut pool, &mut ctx);

        for (i, ann) in pool.annotations.iter().enumerate() {
            // 1-to-1: multipliers vec and sanity vec must have the same length.
            assert_eq!(
                ann.score_trace.multipliers.len(),
                ann.sanity.len(),
                "plan[{i}]: trace.multipliers.len()={} != sanity.len()={}",
                ann.score_trace.multipliers.len(),
                ann.sanity.len(),
            );
            // Each multiplier hit must have kind=Sanity and match the sanity value.
            for (j, (mhit, shit)) in ann
                .score_trace
                .multipliers
                .iter()
                .zip(ann.sanity.iter())
                .enumerate()
            {
                assert_eq!(
                    mhit.kind,
                    MultiplierKind::Sanity,
                    "plan[{i}] multiplier[{j}]: expected kind=Sanity",
                );
                assert!(
                    (mhit.value - shit.multiplier).abs() < 1e-6,
                    "plan[{i}] multiplier[{j}]: value={} != sanity.multiplier={}",
                    mhit.value,
                    shit.multiplier,
                );
            }
            // compute() == ann.score.
            assert!(
                (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                "plan[{i}]: compute()={} != ann.score={}",
                ann.score_trace.compute(),
                ann.score,
            );
        }
    }

    /// On a pool of 3 plans (all finite scores), after apply: for every plan
    /// (ann.score - trace.compute()).abs() < 1e-5.
    #[test]
    fn p3a_sanity_invariant_holds_for_all_finite_plans() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let dest_c = hex_from_offset(3, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
            make_move_plan(vec![dest_c]),
        ];

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(20)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip([0.9_f32, 0.7_f32, 0.5_f32]) {
            ann.score = score;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            ann.score_trace.base = score;
        }
        SanityStage.apply(&mut pool, &mut ctx);

        for (i, ann) in pool.annotations.iter().enumerate() {
            assert!(
                ann.score.is_finite(),
                "plan[{i}]: score must remain finite, got {}", ann.score,
            );
            assert!(
                (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                "plan[{i}]: invariant violated — ann.score={} vs compute()={}",
                ann.score,
                ann.score_trace.compute(),
            );
        }
    }

    /// A plan with ann.score = NEG_INFINITY (masked plan).
    /// After apply: ann.score stays NEG_INFINITY, invariant assert is skipped
    /// (entry.is_finite() == false), no panic. Sanity hits remain empty.
    /// P3a.6: trace.base is NOT set by this stage (Finalize would set it in
    /// production); in isolation the masked plan's trace.base stays as-is.
    #[test]
    fn p3a_sanity_masked_plan_trace_unchanged_or_only_base() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(20)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let mut pool = ScoredPool::new(plans);
        // plan[0] is masked, plan[1] is normal.
        pool.annotations[0].score = f32::NEG_INFINITY;
        pool.annotations[1].score = 0.6;
        // P3a.6: initialise trace.base for the finite plan only.
        pool.annotations[1].score_trace.base = 0.6;

        // Must not panic.
        SanityStage.apply(&mut pool, &mut ctx);

        // plan[0]: score stays NEG_INFINITY;
        // sanity hits remain empty (sanity_adjust_plans skips non-finite).
        let masked = &pool.annotations[0];
        assert_eq!(
            masked.score,
            f32::NEG_INFINITY,
            "masked plan score must remain NEG_INFINITY",
        );
        assert!(
            masked.sanity.is_empty(),
            "masked plan must have no sanity hits, got {:?}", masked.sanity,
        );

        // plan[1]: finite plan invariant holds.
        let normal = &pool.annotations[1];
        assert!(
            normal.score.is_finite(),
            "normal plan score must remain finite",
        );
        assert!(
            (normal.score - normal.score_trace.compute()).abs() < 1e-5,
            "normal plan invariant violated: score={} vs compute()={}",
            normal.score,
            normal.score_trace.compute(),
        );
    }
}
