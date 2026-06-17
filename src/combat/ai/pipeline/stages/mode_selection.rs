//! ModeSelectionStage — selects an `EvaluationMode` per plan via
//! `select_evaluation_modes` and records it in `ann.adaptation`.
//!
//! **Does NOT touch `ann.score` / `ann.factors`.** Score mutation is deferred to
//! `FinalizeStage` (runs next) so `SanityStage`/`CriticsStage` multipliers apply
//! on the finalized base and are never overwritten.
//!
//! ```text
//! Viability → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask
//!          → KillableGate → RepairAffinity → PlanModifiers → PickBest
//! ```
//!
//! `ann.adaptation.original_score` captures `ann.score` at this point — the
//! Default-mode initial score (pre-Finalize). Debug/log only.

use crate::combat::ai::adapt::select_evaluation_modes;
use crate::combat::ai::outcome::AdaptationData;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

pub struct ModeSelectionStage;

impl PlanStage for ModeSelectionStage {
    fn name(&self) -> &'static str {
        "mode_selection"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Collect read-only views for mode selection.
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let adaptation =
            select_evaluation_modes(&pool.plans, &raw_factors, &ctx.intent, ctx.scoring);

        // Write adaptation annotations — do NOT touch ann.score or ann.factors.
        for (i, ann) in pool.annotations.iter_mut().enumerate() {
            if let Some(r) = adaptation.reasons.get(i).and_then(|r| r.as_ref()) {
                ann.adaptation = Some(AdaptationData {
                    reason: r.clone(),
                    // Default-mode initial score (pre-Finalize); see module doc.
                    original_score: ann.score,
                    mode: adaptation.modes[i],
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::scoring::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::test_helpers::{empty_plan, PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn move_plan(dest: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![dest] }],
            final_pos: dest,
            ..TurnPlan::default()
        }
    }

    fn pfv_survival(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::SelfSurvival, v);
        f
    }

    // ── mode_selection_does_not_mutate_score ──────────────────────────────

    #[test]
    fn mode_selection_does_not_mutate_score() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(10)
            .max_hp(20)
            .build();
        let plans = vec![empty_plan(), empty_plan()];
        let pre_scores = [0.7_f32, 0.3_f32];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&pre_scores)
            .factors(raw)
            .build();

        // ── 4. Act ──
        h.run(|ctx| ModeSelectionStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.score, pre_scores[i],
                "ModeSelectionStage must not mutate ann.score[{}]",
                i
            );
        }
    }

    // ── mode_selection_writes_adaptation_for_laststand_plans ──────────────

    #[test]
    fn mode_selection_writes_adaptation_for_laststand_plans() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(10)
            .max_hp(20)
            .build();
        // ProtectSelf with no defensive options → all plans get LastStand.
        let plans = vec![empty_plan(), move_plan(hex_from_offset(1, 0))];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.4])
            .factors(raw)
            .build();

        // ── 4. Act ──
        h.run(|ctx| ModeSelectionStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert!(
                ann.adaptation.is_some(),
                "plan[{}] should have adaptation annotation (ProtectSelfNoDefensive)",
                i
            );
        }
    }

    // ── mode_selection_records_original_score ────────────────────────────

    #[test]
    fn mode_selection_records_original_score() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(10)
            .max_hp(20)
            .build();
        let plans = vec![empty_plan(), empty_plan()];
        let pre_scores = [0.5_f32, 0.4_f32];
        let raw = vec![pfv_survival(0.0), pfv_survival(0.0)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&pre_scores)
            .factors(raw)
            .build();

        // ── 4. Act ──
        h.run(|ctx| ModeSelectionStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            let data = ann.adaptation.as_ref().expect("expected adaptation");
            assert_eq!(
                data.original_score, pre_scores[i],
                "original_score[{}] should match pre-adaptation score",
                i
            );
        }
    }

    // ── mode_selection_adaptation_reason_is_protect_self_no_defensive ────

    #[test]
    fn mode_selection_adaptation_reason_is_protect_self_no_defensive() {
        // ── 1. Test data ──
        // ProtectSelf intent with no defensive plans → adaptation fires with
        // ProtectSelfNoDefensive reason stored directly in ann.adaptation.reason.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(10)
            .max_hp(20)
            .build();
        let plans = vec![empty_plan()];
        let raw = vec![pfv_survival(0.0)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans).scores(&[0.5]).factors(raw).build();

        // ── 4. Act ──
        h.run(|ctx| ModeSelectionStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let adapt = pool.annotations[0]
            .adaptation
            .as_ref()
            .expect("expected adaptation");

        assert!(
            matches!(
                adapt.reason,
                crate::combat::ai::adapt::AdaptationReason::ProtectSelfNoDefensive
            ),
            "expected ProtectSelfNoDefensive, got {:?}",
            adapt.reason,
        );
    }

    // ── mode_selection_no_adaptation_when_no_trigger ──────────────────────

    #[test]
    fn mode_selection_no_adaptation_when_no_trigger() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(20)
            .max_hp(20)
            .build();
        let plans = vec![move_plan(hex_from_offset(1, 0))];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::Reposition;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![PlanFactorValues::default()])
            .build();

        // ── 4. Act ──
        h.run(|ctx| ModeSelectionStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        assert!(
            pool.annotations[0].adaptation.is_none(),
            "expected no adaptation when no trigger fires"
        );
        assert_eq!(pool.annotations[0].score, 0.5);
    }
}
