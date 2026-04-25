//! `PlanStage` pipeline scaffolding — step 7.0.
//!
//! Introduces three foundational types:
//! - `StageCtx` — read-only context threaded through every stage.
//! - `ScoredPool` — typed pool of plans + annotations + scores + raw factors.
//! - `PlanStage` — trait every stage implements.
//! - `Pipeline` — ordered composer that runs stages sequentially.
//!
//! No stages are implemented here; no existing code is wired to this module.
//! Consumers arrive in step 7.1+.

use crate::combat::ai::factors::PlanFactors;
use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;
use crate::core::rng::DiceRng;
use crate::game::hex::Hex;

// ── StageCtx ────────────────────────────────────────────────────────────────

/// Context threaded read-only through every pipeline stage.
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
        }
    }
}

// ── ScoredPool ──────────────────────────────────────────────────────────────

/// Typed pool of plans together with their per-plan annotation, scores, and
/// raw factor decompositions.
///
/// **Invariant:** `plans.len() == annotations.len() == scored.len() ==
/// raw_factors.len()` at all times. Constructors uphold this; stages must
/// preserve it.
pub struct ScoredPool {
    pub plans: Vec<TurnPlan>,
    pub annotations: Vec<PlanAnnotation>,
    pub scored: Vec<f32>,
    pub raw_factors: Vec<PlanFactors>,
}

impl ScoredPool {
    /// Build a pool from a plan list, zero-filling annotation/score/factors.
    pub fn new(plans: Vec<TurnPlan>) -> Self {
        let n = plans.len();
        Self {
            plans,
            annotations: vec![PlanAnnotation::default(); n],
            scored: vec![0.0; n],
            raw_factors: vec![PlanFactors::default(); n],
        }
    }

    /// Empty pool — used on early-return paths where no plans were generated.
    pub fn empty() -> Self {
        Self {
            plans: vec![],
            annotations: vec![],
            scored: vec![],
            raw_factors: vec![],
        }
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }

    /// Iterate plans together with their annotation and score.
    pub fn iter_with_annotation(
        &self,
    ) -> impl Iterator<Item = (&TurnPlan, &PlanAnnotation, f32)> {
        self.plans
            .iter()
            .zip(self.annotations.iter())
            .zip(self.scored.iter().copied())
            .map(|((plan, ann), score)| (plan, ann, score))
    }
}

// ── PlanStage ────────────────────────────────────────────────────────────────

/// A single stage in the scoring pipeline. Each stage mutates `pool` in-place
/// (scores, annotations, or both) and may read/write `ctx` intent fields.
pub trait PlanStage {
    fn name(&self) -> &'static str;
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx);
}

// ── Pipeline ─────────────────────────────────────────────────────────────────

/// Ordered sequence of stages. Runs each stage in insertion order.
pub struct Pipeline {
    stages: Vec<Box<dyn PlanStage>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { stages: vec![] }
    }

    /// Add a stage at the end of the pipeline (builder pattern).
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, stage: Box<dyn PlanStage>) -> Self {
        self.stages.push(stage);
        self
    }

    /// Run all stages in order against `pool` and `ctx`.
    pub fn run(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        for stage in &self.stages {
            stage.apply(pool, ctx);
        }
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::IntentReason;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        UnitBuilder, empty_content, empty_maps, make_scoring_ctx, make_test_ctx,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use std::cell::RefCell;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn dummy_pool(n: usize) -> ScoredPool {
        let plans = vec![TurnPlan::default(); n];
        ScoredPool::new(plans)
    }

    // ── ScoredPool invariant ─────────────────────────────────────────────────

    #[test]
    fn scored_pool_invariant_holds_after_construct() {
        let pool = dummy_pool(2);
        assert_eq!(pool.plans.len(), 2);
        assert_eq!(pool.annotations.len(), 2);
        assert_eq!(pool.scored.len(), 2);
        assert_eq!(pool.raw_factors.len(), 2);
    }

    #[test]
    fn scored_pool_empty_has_zero_len() {
        let pool = ScoredPool::empty();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    // ── Pipeline ordering ────────────────────────────────────────────────────

    thread_local! {
        static CALL_LOG: RefCell<Vec<&'static str>> = const { RefCell::new(vec![]) };
    }

    struct LoggingStage(&'static str);

    impl PlanStage for LoggingStage {
        fn name(&self) -> &'static str {
            self.0
        }
        fn apply(&self, _pool: &mut ScoredPool, _ctx: &mut StageCtx) {
            CALL_LOG.with(|log| log.borrow_mut().push(self.0));
        }
    }

    /// Build a minimal `StageCtx` for tests that only need the pipeline
    /// machinery without caring about scoring values.
    fn with_dummy_ctx<F: FnOnce(StageCtx)>(f: F) {
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            hex_from_offset(0, 0),
            &mut rng,
        );
        f(ctx);
    }

    #[test]
    fn pipeline_runs_stages_in_order() {
        CALL_LOG.with(|log| log.borrow_mut().clear());

        let pipeline = Pipeline::new()
            .add(Box::new(LoggingStage("first")))
            .add(Box::new(LoggingStage("second")));

        let mut pool = dummy_pool(1);
        with_dummy_ctx(|mut ctx| {
            pipeline.run(&mut pool, &mut ctx);
        });

        let order = CALL_LOG.with(|log| log.borrow().clone());
        assert_eq!(order, vec!["first", "second"]);
    }

    #[test]
    fn pipeline_empty_is_noop() {
        let pipeline = Pipeline::new();
        let mut pool = dummy_pool(2);

        with_dummy_ctx(|mut ctx| {
            pipeline.run(&mut pool, &mut ctx);
        });

        // pool unchanged — all zeros
        assert!(pool.scored.iter().all(|&s| s == 0.0));
        assert_eq!(pool.len(), 2);
    }
}
