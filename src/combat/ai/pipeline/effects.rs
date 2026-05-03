//! Score Effect Engine — drive-loop infrastructure for score-effect stages.
//!
//! Stages (Modifiers, Sanity, Critics, ProtectSelfMask, KillableGate) emit
//! `EmittedEffect`s via `ScoreEffectStage::compute_effects`. The drive-loop
//! wraps each into `AppliedEffect` (adding `source: StageId`) and applies them
//! via `PlanAnnotation::apply_effect`, which is the SOLE writer of:
//!   - `score_trace` (multipliers / addends / masks / gates)
//!   - legacy observability (`modifiers`, `sanity`, `critics`, `contract`)
//!   - cached `score` (recomputed from `score_trace.compute()` at end of stage)
//!
//! In Step 1, this infrastructure exists but no production stage uses it.
//! Steps 2-6 migrate stages one-by-one. Step 7 finalizes privatization.

use crate::combat::ai::outcome::ContractMaskHit;
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::{AddendHit, GateHit, MaskHit, MultiplierHit};
use crate::combat::ai::pipeline::stages::critics::CriticHit;
use crate::combat::ai::pipeline::stages::modifiers::ModifierContribution;
use crate::combat::ai::pipeline::stages::sanity::SanityHit;
use crate::combat::ai::pipeline::{ScoredPool, StageCtx};

/// One score-affecting hit. Pushed into `ann.score_trace` by drive-loop.
#[derive(Clone, Debug)]
pub enum ScoreHit {
    Multiplier(MultiplierHit),
    Addend(AddendHit),
    Mask(MaskHit),
    Gate(GateHit),
}

/// Companion legacy observability paired with a `ScoreHit`.
/// Each variant has a fixed allowed pairing with `ScoreHit` (validated at apply time).
#[derive(Clone, Debug)]
pub enum EffectObservation {
    /// Pairs with `ScoreHit::Addend`. Pushed into `ann.modifiers`.
    Modifier(ModifierContribution),
    /// Pairs with `ScoreHit::Multiplier`. Pushed into `ann.sanity`.
    Sanity(SanityHit),
    /// Pairs with `ScoreHit::Multiplier`. Pushed into `ann.critics`.
    Critic(CriticHit),
    /// Pairs with `ScoreHit::Mask`. Set on `ann.contract` (Option overwrite).
    Contract(ContractMaskHit),
}

/// What a stage emits. Has NO `source` — drive-loop wraps with `AppliedEffect`.
#[derive(Clone, Debug)]
pub struct EmittedEffect {
    pub plan_index: usize,
    pub hit: ScoreHit,
    pub observability: Option<EffectObservation>,
}

/// What drive-loop applies. `source` is added by drive-loop from `stage.id()`
/// — stages cannot lie about origin.
#[derive(Clone, Debug)]
pub struct AppliedEffect {
    pub source: StageId,
    pub plan_index: usize,
    pub hit: ScoreHit,
    pub observability: Option<EffectObservation>,
}

/// Trait for score-effect stages migrating to the engine.
///
/// Implementors compute effects from `(ctx, pool)` — they MUST NOT mutate
/// pool/annotations directly. Drive-loop applies effects via
/// `apply_score_effect_stage`.
pub trait ScoreEffectStage {
    /// Stable identifier — used by drive-loop to wrap `EmittedEffect → AppliedEffect`.
    fn id(&self) -> StageId;

    /// Compute all effects for all plans in the pool. Per-pool setup
    /// (e.g. `build_summon_dpr_cache` for Modifiers) happens inside this method.
    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect>;
}

/// Run a `ScoreEffectStage`: compute emitted effects → wrap with source →
/// apply each via `PlanAnnotation::apply_effect` → recompute `ann.score`
/// from trace for every annotation.
///
/// This is the sole writer of `score_trace`, legacy observability fields,
/// and the cached `score` for stages that opt into the engine.
pub fn apply_score_effect_stage<S: ScoreEffectStage + ?Sized>(
    stage: &S,
    pool: &mut ScoredPool,
    ctx: &mut StageCtx,
) {
    let emitted = stage.compute_effects(ctx, pool);
    let source = stage.id();
    for e in emitted {
        debug_assert!(
            e.plan_index < pool.annotations.len(),
            "ScoreEffectStage::compute_effects emitted plan_index {} ≥ pool len {}",
            e.plan_index,
            pool.annotations.len(),
        );
        let applied = AppliedEffect {
            source,
            plan_index: e.plan_index,
            hit: e.hit,
            observability: e.observability,
        };
        pool.annotations[applied.plan_index].apply_effect(&applied);
    }
    // Recompute cached score from trace for every plan.
    // Important: applies even to plans this stage didn't touch — preserves
    // invariant `ann.score == ann.score_trace.compute()` after each stage.
    for ann in pool.annotations.iter_mut() {
        ann.recompute_score_from_trace();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::score_trace::{
        AddendHit, GateHit, GateOutcome, MaskHit, MaskKind, MultiplierHit, MultiplierKind,
    };
    use crate::combat::ai::pipeline::stages::modifiers::ModifierContribution;
    use crate::combat::ai::pipeline::stages::sanity::{SanityHit, SanityRule};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::Hex;

    struct TestStage {
        id: StageId,
        effects: Vec<EmittedEffect>,
    }

    impl ScoreEffectStage for TestStage {
        fn id(&self) -> StageId {
            self.id
        }

        fn compute_effects(&self, _ctx: &StageCtx, _pool: &ScoredPool) -> Vec<EmittedEffect> {
            self.effects.clone()
        }
    }

    fn make_actor() -> crate::combat::ai::world::snapshot::UnitSnapshot {
        UnitBuilder::new(1, Team::Enemy, Hex::ZERO).build()
    }

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

    #[test]
    fn addend_effect_updates_trace_and_modifiers_and_score() {
        let actor = make_actor();
        let h = StageTestHarness::new(actor);
        h.run(|ctx| {
            let mut pool = PoolBuilder::new(vec![empty_plan()])
                .scores(&[1.0])
                .trace_base_eq_score()
                .build();

            let addend_value = 0.5_f32;
            let stage = TestStage {
                id: StageId::PlanModifiers,
                effects: vec![EmittedEffect {
                    plan_index: 0,
                    hit: ScoreHit::Addend(AddendHit { name: "test_bonus", value: addend_value }),
                    observability: Some(EffectObservation::Modifier(ModifierContribution {
                        name: "test_bonus".to_string(),
                        contribution: addend_value,
                    })),
                }],
            };

            apply_score_effect_stage(&stage, &mut pool, ctx);

            let ann = &pool.annotations[0];
            assert_eq!(ann.score_trace.addends.len(), 1, "addend pushed to trace");
            assert_eq!(ann.modifiers.len(), 1, "modifier pushed to legacy field");
            assert!(
                (ann.score - (1.0 + addend_value)).abs() < 1e-6,
                "score == base + addend: got {}",
                ann.score
            );
        });
    }

    #[test]
    fn multiplier_effect_with_sanity_observation() {
        let actor = make_actor();
        let h = StageTestHarness::new(actor);
        h.run(|ctx| {
            let mut pool = PoolBuilder::new(vec![empty_plan()])
                .scores(&[1.0])
                .trace_base_eq_score()
                .build();

            let stage = TestStage {
                id: StageId::Sanity,
                effects: vec![EmittedEffect {
                    plan_index: 0,
                    hit: ScoreHit::Multiplier(MultiplierHit {
                        kind: MultiplierKind::Sanity,
                        value: 0.5,
                    }),
                    observability: Some(EffectObservation::Sanity(SanityHit {
                        rule: SanityRule::HealerExposure,
                        multiplier: 0.5,
                    })),
                }],
            };

            apply_score_effect_stage(&stage, &mut pool, ctx);

            let ann = &pool.annotations[0];
            assert_eq!(ann.score_trace.multipliers.len(), 1, "multiplier pushed to trace");
            assert_eq!(ann.sanity.len(), 1, "sanity hit pushed to legacy field");
            assert!(
                (ann.score - 0.5).abs() < 1e-6,
                "score == base * 0.5: got {}",
                ann.score
            );
        });
    }

    #[test]
    fn mask_effect_makes_score_neg_inf() {
        let actor = make_actor();
        let h = StageTestHarness::new(actor);
        h.run(|ctx| {
            let mut pool = PoolBuilder::new(vec![empty_plan()])
                .scores(&[1.0])
                .trace_base_eq_score()
                .build();

            let stage = TestStage {
                id: StageId::ProtectSelfMask,
                effects: vec![EmittedEffect {
                    plan_index: 0,
                    hit: ScoreHit::Mask(MaskHit { kind: MaskKind::Poison, source: "test" }),
                    observability: None,
                }],
            };

            apply_score_effect_stage(&stage, &mut pool, ctx);

            let ann = &pool.annotations[0];
            assert_eq!(ann.score_trace.masks.len(), 1, "mask pushed to trace");
            assert_eq!(
                ann.score,
                f32::NEG_INFINITY,
                "score == NEG_INFINITY after poison mask"
            );
        });
    }

    #[test]
    fn gate_plus_mask_double_emit_preserved() {
        let actor = make_actor();
        let h = StageTestHarness::new(actor);
        h.run(|ctx| {
            let mut pool = PoolBuilder::new(vec![empty_plan()])
                .scores(&[1.0])
                .trace_base_eq_score()
                .build();

            // Imitate KillableGate double-emit: Mask + Gate on the same plan.
            let stage = TestStage {
                id: StageId::KillableGate,
                effects: vec![
                    EmittedEffect {
                        plan_index: 0,
                        hit: ScoreHit::Mask(MaskHit {
                            kind: MaskKind::Poison,
                            source: "killable_gate",
                        }),
                        observability: None,
                    },
                    EmittedEffect {
                        plan_index: 0,
                        hit: ScoreHit::Gate(GateHit {
                            outcome: GateOutcome::Reject,
                            source: "killable_gate",
                        }),
                        observability: None,
                    },
                ],
            };

            apply_score_effect_stage(&stage, &mut pool, ctx);

            let ann = &pool.annotations[0];
            assert_eq!(ann.score_trace.masks.len(), 1, "mask in trace");
            assert_eq!(ann.score_trace.gates.len(), 1, "gate in trace");
            assert_eq!(ann.score, f32::NEG_INFINITY, "score == NEG_INFINITY");
            assert!(ann.score_trace.is_gated(), "is_gated() == true");
        });
    }

    #[test]
    #[should_panic(expected = "invalid score effect pairing")]
    fn invalid_pairing_panics() {
        let actor = make_actor();
        let h = StageTestHarness::new(actor);
        h.run(|ctx| {
            let mut pool = PoolBuilder::new(vec![empty_plan()])
                .scores(&[1.0])
                .trace_base_eq_score()
                .build();

            // Addend paired with Sanity observation — illegal combination.
            let stage = TestStage {
                id: StageId::PlanModifiers,
                effects: vec![EmittedEffect {
                    plan_index: 0,
                    hit: ScoreHit::Addend(AddendHit { name: "bad", value: 0.1 }),
                    observability: Some(EffectObservation::Sanity(SanityHit {
                        rule: SanityRule::RetreatTrap,
                        multiplier: 0.9,
                    })),
                }],
            };

            apply_score_effect_stage(&stage, &mut pool, ctx);
        });
    }
}
