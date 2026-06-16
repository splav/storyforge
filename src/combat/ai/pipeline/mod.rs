//! `PlanStage` pipeline scaffolding.
//!
//! Foundational types:
//! - `StageCtx` — context threaded through every pipeline stage.
//! - `ScoredPool` — typed pool of plans + annotations (`score` and
//!   `factors: PlanFactorValues` are fields of `PlanAnnotation`).
//! - `PlanStage` — trait every stage implements.
//!
//! The production stage order lives in `order::PRODUCTION_PIPELINE` (single
//! source of truth).  Use `order::run` to execute any pipeline slice.

pub mod effects;
pub mod order;
pub mod score_trace;
pub mod spec;
pub mod stages;

use crate::combat::ai::intent::agenda::Agenda;
use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::TurnPlan;
use crate::game::hex::Hex;
use combat_engine::DiceRng;

// ── StageCtx ────────────────────────────────────────────────────────────────

/// Context threaded through every pipeline stage.
///
/// Lifetimes mirror `ScoringCtx<'w, 'p>`:
/// - `'w` — borrow of world/map/reservations references inside `ScoringCtx`.
/// - `'s` — borrow of the outer `pick_action` stack frame (`ScoringCtx`,
///   intent, rng). `'s` is shorter-lived than `'w`.
pub struct StageCtx<'w, 's> {
    pub scoring: &'s ScoringCtx<'w, 's>,
    pub intent: TacticalIntent,
    pub intent_reason: IntentReason,
    pub actor_pos: Hex,
    pub rng: &'s mut DiceRng,
    /// Step 11.4: optional agenda for per-item composition.
    /// `Some` when `pick_action` has built a non-empty agenda; `None` on legacy
    /// / empty-agenda paths so that `ItemScoringStage` and `PickBestStage` can
    /// gracefully fall back to single-intent behaviour.
    pub agenda: Option<&'s Agenda>,
}

impl<'w, 's> StageCtx<'w, 's> {
    pub fn new(
        scoring: &'s ScoringCtx<'w, 's>,
        intent: TacticalIntent,
        intent_reason: IntentReason,
        actor_pos: Hex,
        rng: &'s mut DiceRng,
    ) -> Self {
        Self {
            scoring,
            intent,
            intent_reason,
            actor_pos,
            rng,
            agenda: None,
        }
    }

    /// Attach an agenda to this context.  Called from `pick_action` after
    /// `build_agenda` returns a non-empty agenda.
    pub fn with_agenda(mut self, agenda: &'s Agenda) -> Self {
        self.agenda = Some(agenda);
        self
    }
}

// ── ScoredPool ──────────────────────────────────────────────────────────────

/// Typed pool of plans together with their per-plan annotations.
///
/// **Invariant:** `plans.len() == annotations.len()` at all times — constructors
/// uphold it, stages must preserve it. `score` / `raw_factors` live inside
/// `PlanAnnotation`, not in parallel vecs.
pub struct ScoredPool {
    pub plans: Vec<TurnPlan>,
    pub annotations: Vec<PlanAnnotation>,
}

impl ScoredPool {
    /// Build a pool from a plan list, zero-filling annotations.
    pub fn new(plans: Vec<TurnPlan>) -> Self {
        let n = plans.len();
        Self {
            plans,
            annotations: vec![PlanAnnotation::default(); n],
        }
    }

    /// Empty pool — used on early-return paths where no plans were generated.
    pub fn empty() -> Self {
        Self {
            plans: vec![],
            annotations: vec![],
        }
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }

    /// Iterate plans together with their annotation and score.
    pub fn iter_with_annotation(&self) -> impl Iterator<Item = (&TurnPlan, &PlanAnnotation, f32)> {
        self.plans
            .iter()
            .zip(self.annotations.iter())
            .map(|(plan, ann)| (plan, ann, ann.score))
    }

    /// Authoritative outcomes for plan at `idx` — live on `TurnPlan.annotation`.
    /// `pool.annotations[i].outcomes` is dead during the pipeline (populated only
    /// at log time), so stages and critics MUST read through this accessor.
    pub fn plan_outcomes(
        &self,
        idx: usize,
    ) -> &[crate::combat::ai::outcome::ActionOutcomeEstimate] {
        self.plans[idx].annotation.outcomes.as_slice()
    }
}

// ── PlanStage ────────────────────────────────────────────────────────────────

/// A single stage in the scoring pipeline. Each stage mutates `pool` in-place
/// (scores, annotations, or both) and may read/write `ctx` intent fields.
pub trait PlanStage {
    fn name(&self) -> &'static str;
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::plan::types::TurnPlan;

    fn dummy_pool(n: usize) -> ScoredPool {
        let plans = vec![TurnPlan::default(); n];
        ScoredPool::new(plans)
    }

    #[test]
    fn scored_pool_invariant_holds_after_construct() {
        let pool = dummy_pool(2);
        assert_eq!(pool.plans.len(), 2);
        assert_eq!(pool.annotations.len(), 2);
    }

    #[test]
    fn scored_pool_empty_has_zero_len() {
        let pool = ScoredPool::empty();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    /// Pipeline-order regression: `PlanModifiersStage` must run after
    /// `RepairAffinityStage` and before `PickBestStage`, so every plan ends with
    /// one addend per registered modifier in canonical order. Reordering it past
    /// `PickBestStage` would leave the chosen plan's `modifiers` empty.
    #[test]
    fn pipeline_runs_modifiers_after_repair_before_pick() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        use crate::combat::ai::intent::{IntentReason, TacticalIntent};
        use crate::combat::ai::pipeline::order::{run, PRODUCTION_PIPELINE};
        use crate::combat::ai::pipeline::stages::modifiers::PLAN_MODIFIERS;
        use crate::combat::ai::world::reservations::Reservations;

        use crate::combat::ai::test_helpers::{
            empty_maps, make_scoring_ctx, make_test_ctx, snapshot_from, UnitBuilder,
        };
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::DiceRng;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = crate::combat::ai::test_helpers::empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(vec![TurnPlan::default(), TurnPlan::default()]);
        // Give plans non-masked scores so PlanModifiersStage processes them.
        pool.annotations[0].score = 1.0;
        pool.annotations[1].score = 0.5;

        run(PRODUCTION_PIPELINE, &mut pool, &mut ctx);

        // Exactly one chosen plan.
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");

        // Every non-masked plan (both plans here) should have exactly PLAN_MODIFIERS.len()
        // addend hits in score_trace, populated by PlanModifiersStage.
        let expected_modifier_count = PLAN_MODIFIERS.len();
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.score_trace.addends.len(),
                expected_modifier_count,
                "plan[{i}] must have {expected_modifier_count} addend entries in trace, got {}",
                ann.score_trace.addends.len()
            );
        }

        // Addend names must appear in canonical PLAN_MODIFIERS order.
        let chosen = pool.annotations.iter().find(|a| a.chosen).unwrap();
        assert_eq!(chosen.score_trace.addends[0].name, "summon_bonus");
        assert_eq!(chosen.score_trace.addends[1].name, "trade_bonus");
        assert_eq!(chosen.score_trace.addends[2].name, "repair_bonus");
    }

    /// P3a.6: after the full pipeline, `ann.score == ann.score_trace.compute()`
    /// for every finite-score plan. Catches any stage that resets the score or
    /// forgets to push its trace hit (trace accumulates from `FinalizeStage::base`).
    #[test]
    fn p3a_full_pipeline_trace_compute_equals_ann_score() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        use crate::combat::ai::intent::{IntentReason, TacticalIntent};
        use crate::combat::ai::pipeline::order::{run, PRODUCTION_PIPELINE};
        use crate::combat::ai::world::reservations::Reservations;

        use crate::combat::ai::test_helpers::{
            empty_content, empty_maps, make_scoring_ctx, make_test_ctx, snapshot_from, UnitBuilder,
        };
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::DiceRng;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
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
            actor.pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(vec![
            crate::combat::ai::plan::types::TurnPlan::default(),
            crate::combat::ai::plan::types::TurnPlan::default(),
        ]);
        pool.annotations[0].score = 1.0;
        pool.annotations[1].score = 0.5;

        run(PRODUCTION_PIPELINE, &mut pool, &mut ctx);

        for (i, ann) in pool.annotations.iter().enumerate() {
            if ann.score.is_finite() {
                let computed = ann.score_trace.compute();
                assert!(
                    (ann.score - computed).abs() < 1e-5,
                    "P3a.6: plan[{i}] ann.score={} vs trace.compute()={} — invariant violated",
                    ann.score,
                    computed,
                );
            }
        }
    }
}
