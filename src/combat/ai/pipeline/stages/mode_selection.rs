//! ModeSelectionStage — step 11.0.
//!
//! Selects an
//! `EvaluationMode` for each plan via `select_evaluation_modes` and records
//! the decision in `ann.adaptation`.
//!
//! **Critically, this stage does NOT touch `ann.score` or `ann.factors`.**
//! Score mutation is deferred to `FinalizeStage`, which runs immediately
//! after this stage in the pipeline. This ordering ensures that
//! `SanityStage` and `CriticsStage`, which apply multiplicative modifiers
//! to `ann.score`, execute on the already-finalized base score and are
//! never overwritten.
//!
//! # Pipeline position (step 11.0)
//!
//! ```text
//! Viability → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask
//!          → KillableGate → RepairAffinity → PlanModifiers → PickBest
//! ```
//!
//! # `ann.adaptation.original_score` semantics
//!
//! The `original_score` field is set to `ann.score` at the time this stage
//! runs — which is the **Default-mode initial score** (post-Viability,
//! pre-Finalize). In prior pipeline order (step 7.2) this field captured
//! the post-Sanity/Critics score. The field is used only for debug/log
//! purposes so the semantic change is benign; it is documented here for
//! clarity.

use crate::combat::ai::outcome::AdaptationData;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::adapt::select_evaluation_modes;

pub struct ModeSelectionStage;

impl PlanStage for ModeSelectionStage {
    fn name(&self) -> &'static str {
        "mode_selection"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Collect read-only views for mode selection.
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let adaptation = select_evaluation_modes(
            &pool.plans,
            &raw_factors,
            &ctx.intent,
            ctx.scoring,
        );

        // Write adaptation annotations — do NOT touch ann.score or ann.factors.
        for (i, ann) in pool.annotations.iter_mut().enumerate() {
            if let Some(r) = adaptation.reasons.get(i).and_then(|r| r.as_ref()) {
                ann.adaptation = Some(AdaptationData {
                    reason: r.clone(),
                    // original_score here is the Default-mode initial score
                    // (post-Viability, pre-Finalize). See module doc for semantics.
                    original_score: ann.score,
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

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

    /// ModeSelectionStage must not change ann.score for any plan, including
    /// those that triggered LastStand mode.
    #[test]
    fn mode_selection_does_not_mutate_score() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
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

    /// Plans that trigger LastStand must have ann.adaptation = Some(_) after
    /// ModeSelectionStage.
    #[test]
    fn mode_selection_writes_adaptation_for_laststand_plans() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
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

    /// When adaptation triggers, ann.adaptation.original_score must equal
    /// the pre-adaptation ann.score.
    #[test]
    fn mode_selection_records_original_score() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
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

    /// IntentReason::Adapted built from ann.adaptation encodes the correct
    /// AdaptationReason (parity with legacy adaptation_data_round_trips_through_intent_reason).
    #[test]
    fn mode_selection_adaptation_reason_is_protect_self_no_defensive() {
        // ── 1. Test data ──
        // ProtectSelf intent with no defensive plans → adaptation fires with
        // ProtectSelfNoDefensive reason stored directly in ann.adaptation.reason.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let plans = vec![empty_plan()];
        let raw = vec![pfv_survival(0.0)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(raw)
            .build();

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

    /// Healthy actor with no AoO threats under non-ProtectSelf intent →
    /// no adaptation annotation written.
    #[test]
    fn mode_selection_no_adaptation_when_no_trigger() {
        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(20).max_hp(20).build();
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
