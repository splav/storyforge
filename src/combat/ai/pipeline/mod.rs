//! `PlanStage` pipeline scaffolding.
//!
//! Foundational types:
//! - `StageCtx` — context threaded through every stage.
//! - `ScoredPool` — typed pool of plans + annotations (`score` and
//!   `factors: PlanFactorValues` are fields of `PlanAnnotation`).
//! - `PlanStage` — trait every stage implements.
//!
//! All stages run via `run_pool_pipeline`.

pub mod stages;

use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;
use crate::core::rng::DiceRng;
use crate::game::hex::Hex;

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

/// Typed pool of plans together with their per-plan annotations.
///
/// **Invariant:** `plans.len() == annotations.len()` at all times.
/// Constructors uphold this; stages must preserve it.
///
/// Step 7.4: `score` and `raw_factors` live inside `PlanAnnotation` rather
/// than in separate parallel vecs. Callers read/write `annotations[i].score`
/// and `annotations[i].raw_factors` directly.
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
    pub fn iter_with_annotation(
        &self,
    ) -> impl Iterator<Item = (&TurnPlan, &PlanAnnotation, f32)> {
        self.plans
            .iter()
            .zip(self.annotations.iter())
            .map(|(plan, ann)| (plan, ann, ann.score))
    }
}

// ── PlanStage ────────────────────────────────────────────────────────────────

/// A single stage in the scoring pipeline. Each stage mutates `pool` in-place
/// (scores, annotations, or both) and may read/write `ctx` intent fields.
pub trait PlanStage {
    fn name(&self) -> &'static str;
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx);
}

// ── run_pool_pipeline ────────────────────────────────────────────────────────

/// Run all scoring and selection stages in order.
///
/// Compile-time stage order, zero trait-object indirection:
/// Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate →
/// RepairAffinity → PlanModifiers → PickBest.
///
/// `PlanModifiersStage` applies the three registered post-normalisation modifiers
/// (summon_bonus, trade_bonus, repair_bonus) after repair affinity is populated
/// and before the winner is selected.
///
/// After this returns, exactly one annotation has `chosen = true` and its
/// `pick` field populated (unless the pool is empty).
pub fn run_pool_pipeline(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use stages::{
        adaptation::AdaptationStage,
        killable_gate::KillableGateStage,
        pick_best::PickBestStage,
        plan_modifiers::PlanModifiersStage,
        protect_self::ProtectSelfMaskStage,
        repair_affinity::RepairAffinityStage,
        sanity::SanityStage,
        viability::ViabilityStage,
    };
    ViabilityStage.apply(pool, ctx);
    SanityStage.apply(pool, ctx);
    AdaptationStage.apply(pool, ctx);
    ProtectSelfMaskStage.apply(pool, ctx);
    KillableGateStage.apply(pool, ctx);
    RepairAffinityStage.apply(pool, ctx);
    PlanModifiersStage.apply(pool, ctx);
    PickBestStage.apply(pool, ctx);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::planning::types::TurnPlan;

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

    /// Verify that `run_pool_pipeline` applies `PlanModifiersStage` (after
    /// `RepairAffinityStage` and before `PickBestStage`) by checking that the
    /// chosen plan's `annotation.modifiers` has exactly one entry per registered
    /// modifier (i.e. PLAN_MODIFIERS.len() == 3), in fixed order.
    ///
    /// This is a pipeline-order regression test: if `PlanModifiersStage` were
    /// removed or reordered after `PickBestStage`, the chosen plan would have
    /// an empty `modifiers` vec.
    #[test]
    fn pipeline_runs_modifiers_after_repair_before_pick() {
        use crate::combat::ai::difficulty::DifficultyProfile;
        use crate::combat::ai::intent::{IntentReason, TacticalIntent};
        use crate::combat::ai::modifiers::PLAN_MODIFIERS;
        use crate::combat::ai::reservations::Reservations;
        use crate::combat::ai::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{
            empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
        };
        use crate::core::DiceRng;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
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

        run_pool_pipeline(&mut pool, &mut ctx);

        // Exactly one chosen plan.
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");

        // Every non-masked plan (both plans here) should have exactly PLAN_MODIFIERS.len()
        // modifier entries populated by PlanModifiersStage.
        let expected_modifier_count = PLAN_MODIFIERS.len();
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.modifiers.len(),
                expected_modifier_count,
                "plan[{i}] must have {expected_modifier_count} modifier entries, got {}",
                ann.modifiers.len()
            );
        }

        // Modifier names must appear in canonical PLAN_MODIFIERS order.
        let chosen = pool.annotations.iter().find(|a| a.chosen).unwrap();
        assert_eq!(chosen.modifiers[0].name, "summon_bonus");
        assert_eq!(chosen.modifiers[1].name, "trade_bonus");
        assert_eq!(chosen.modifiers[2].name, "repair_bonus");
    }
}
