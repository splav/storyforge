//! Post-normalisation plan modifiers — pipeline stage 8.B.
//!
//! This module contains both the `PlanModifier` trait + associated types and the
//! `PlanModifiersStage` dispatcher that runs them.
//!
//! `PlanModifier` trait: each modifier contributes a signed addendum to
//! `ann.score` after the full factor/terminal scoring pass. Three built-in
//! modifiers are registered in `PLAN_MODIFIERS` (apply order is fixed):
//!
//! 1. `summon_bonus` — scarce-resource bonus for Summon plans.
//! 2. `trade_bonus`  — economic gain/loss relative to actor value.
//! 3. `repair_bonus` — goal-affinity amplifier when a stored goal is present.
//!
//! `PlanModifiersStage` applies all registered `PlanModifier` implementations
//! to every non-masked plan in the pool. This stage runs after `RepairAffinityStage`
//! (which populates `ann.repair_affinity`) and before `PickBestStage` (which reads
//! the final `ann.score`).
//!
//! For each plan the stage iterates `PLAN_MODIFIERS` in fixed order
//! `[summon_bonus, trade_bonus, repair_bonus]`, accumulates the signed
//! additive contribution into `ann.score`, and records each contribution in
//! `ann.modifiers` for observability.
//!
//! Masked plans (`ann.score == NEG_INFINITY`) are skipped entirely so that
//! contract masks applied by `ProtectSelfMaskStage` / `KillableGateStage`
//! are not disturbed.
//!
//! **P3a.1 / P3a.6:** Each modifier contribution is also pushed as an
//! `AddendHit` into `ann.score_trace`. After P3a.6 cleanup the trace
//! accumulates over the full pipeline: `FinalizeStage` sets `trace.base`,
//! downstream stages (Sanity, Critics, Modifiers) push hits on top.
//! Invariant after apply: `ann.score == trace.compute()`.

pub mod repair_bonus;
pub mod summon_bonus;
pub mod trade_bonus;

use crate::combat::ai::pipeline::effects::{
    apply_score_effect_stage, EffectObservation, EmittedEffect, ScoreEffectStage, ScoreHit,
};
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::AddendHit;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::scoring::factors::aggregate::build_summon_dpr_cache;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::repair::RepairWeights;
use crate::combat::ai::scoring::trade::unit_value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A post-normalisation additive modifier applied to every plan's score.
///
/// Implementations must be `Sync` so they can live in a `static` slice.
/// All three built-in implementations are zero-state unit structs —
/// `Sync` is satisfied automatically.
pub trait PlanModifier: Sync {
    /// Stable identifier for logging and debug overlays.
    fn name(&self) -> &'static str;

    /// Compute the signed additive contribution for one plan.
    ///
    /// Returns `0.0` when the modifier does not apply (e.g. no Summon steps,
    /// no stored goal). Positive values increase `ann.score`; negative values
    /// decrease it.
    fn modify(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ModifierCtx<'_, '_, '_>,
    ) -> f32;
}

// ── Context ──────────────────────────────────────────────────────────────────

/// Read-only context passed to every `PlanModifier::modify` call.
///
/// Lifetime parameters:
/// - `'w` — world/map borrows inside `StageCtx::scoring` (`AiWorld`, `InfluenceMaps`).
/// - `'s` — outer `pick_action` stack-frame borrow (`ScoringCtx`, intent, rng).
/// - `'a` — the borrow of `StageCtx` itself (shorter than both `'w` and `'s`).
pub struct ModifierCtx<'w, 's, 'a> {
    pub stage: &'a StageCtx<'w, 's>,
    /// Pre-computed per-template summon DPR cache, built once per pool in
    /// `PlanModifiersStage`. Empty when no plan summons.
    pub summon_dpr: &'a HashMap<String, f32>,
    /// `unit_value(active, world.content)` — computed once per pool.
    pub actor_value: f32,
    /// Role-mixed repair weights for the active actor. Computed once per pool
    /// via `active.role.repair_weights(world.tuning)`.
    pub repair_weights: RepairWeights,
}

// ── Contribution record ───────────────────────────────────────────────────────

/// Per-modifier additive contribution stored in `PlanAnnotation.modifiers`.
///
/// Populated by `PlanModifiersStage` for each plan. `name` matches the
/// `PlanModifier::name()` return value; `contribution` is the signed addendum
/// applied to `ann.score`. Entries appear in `PLAN_MODIFIERS` order.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ModifierContribution {
    pub name: String,
    pub contribution: f32,
}

// ── Static registry ──────────────────────────────────────────────────────────

/// Ordered slice of all active plan modifiers.
///
/// Order is fixed: `[summon_bonus, trade_bonus, repair_bonus]`.
/// `PlanModifiersStage` applies them left-to-right; the same order
/// appears in `PlanAnnotation.modifiers` entries.
pub static PLAN_MODIFIERS: &[&dyn PlanModifier] = &[
    &summon_bonus::MODIFIER,
    &trade_bonus::MODIFIER,
    &repair_bonus::MODIFIER,
];

// ── PlanModifiersStage ────────────────────────────────────────────────────────

pub struct PlanModifiersStage;

impl ScoreEffectStage for PlanModifiersStage {
    fn id(&self) -> StageId {
        StageId::PlanModifiers
    }

    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect> {
        // Per-pool setup — migrated from the old apply() body.
        let summon_dpr = build_summon_dpr_cache(&pool.plans, ctx.scoring.world);
        let actor_value = unit_value(ctx.scoring.active, ctx.scoring.world.content);
        let repair_weights = ctx.scoring.active.role.repair_weights(ctx.scoring.world.tuning);

        let mctx = ModifierCtx {
            stage: ctx,
            summon_dpr: &summon_dpr,
            actor_value,
            repair_weights,
        };

        let mut emitted = Vec::new();
        for (plan_index, (plan, ann)) in pool.plans.iter().zip(pool.annotations.iter()).enumerate() {
            // Skip plans masked by ProtectSelf / KillableGate.
            if !ann.is_selectable() {
                continue;
            }
            for m in PLAN_MODIFIERS {
                let contribution = m.modify(plan, ann, &mctx);
                emitted.push(EmittedEffect {
                    plan_index,
                    hit: ScoreHit::Addend(AddendHit { name: m.name(), value: contribution }),
                    observability: Some(EffectObservation::Modifier(ModifierContribution {
                        name: m.name().into(),
                        contribution,
                    })),
                });
            }
        }
        emitted
    }
}

impl PlanStage for PlanModifiersStage {
    fn name(&self) -> &'static str {
        "plan_modifiers"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_score_effect_stage(self, pool, ctx);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::order::{run, PRODUCTION_PIPELINE};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    // ── PlanModifiersStage integration tests via PRODUCTION_PIPELINE ─────────

    /// After apply(), each non-masked plan has exactly PLAN_MODIFIERS.len()
    /// addend hits in score_trace, in PLAN_MODIFIERS order.
    #[test]
    fn p3a_modifiers_push_addends_to_trace() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0, 0.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| run(PRODUCTION_PIPELINE, &mut pool, ctx));

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            assert_eq!(
                ann.score_trace.addends.len(),
                PLAN_MODIFIERS.len(),
                "plan[{i}] trace.addends.len() must equal PLAN_MODIFIERS.len()"
            );
            for (j, hit) in ann.score_trace.addends.iter().enumerate() {
                assert_eq!(
                    hit.name, PLAN_MODIFIERS[j].name(),
                    "plan[{i}] addend[{j}].name mismatch"
                );
            }
        }
    }

    /// A masked plan (Poison mask in trace) must not receive modifier addends.
    /// After apply(), score_trace.addends stays empty and is_selectable() stays false.
    /// score is finite (Step 3: compute() ignores masks) — selectability is
    /// communicated via trace flags, not score magnitude.
    ///
    /// This mirrors production: ProtectSelfMaskStage / KillableGateStage emit
    /// `ScoreHit::Mask(MaskHit { kind: MaskKind::Poison, .. })` before
    /// PlanModifiersStage runs.
    #[test]
    fn p3a_modifiers_masked_plan_trace_unchanged() {
        use crate::combat::ai::pipeline::score_trace::{MaskHit, MaskKind};

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool — masked plan with Poison mask in trace.
        //    This mirrors the post-ProtectSelfMaskStage / KillableGateStage state.
        let mut pool = PoolBuilder::new(plans)
            .customize(|anns| {
                anns[0].score_trace.push_mask(MaskHit { kind: MaskKind::Poison, source: "test", original_score: None });
            })
            .build();

        // ── 4. Act — call the stage directly.
        h.run(|ctx| PlanModifiersStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert!(ann.score_trace.is_masked(), "mask must remain in trace");
        assert!(!ann.is_selectable(), "masked plan must not be selectable");
        assert_eq!(ann.score_trace.base, 0.0, "masked plan trace.base must stay 0");
        assert!(ann.score_trace.addends.is_empty(), "masked plan trace.addends must stay empty");
        // score is finite after Step 3 cutover
        assert!(ann.score.is_finite(), "score is finite after Step 3 cutover");
    }
}
