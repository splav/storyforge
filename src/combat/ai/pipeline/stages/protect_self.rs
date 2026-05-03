//! ProtectSelfMaskStage — step 7.2.
//!
//! Replicates `PlanRanking::apply_protect_self` as a `PlanStage` with an
//! **internal predicate**: the stage skips entirely when `ctx.intent` is not
//! `ProtectSelf`. This removes the `if matches!(ranking.intent, ProtectSelf)`
//! guard from `pick_action` body.
//!
//! Writes `annotation.contract = Some(ContractMaskHit { mask: "protect_self", … })`
//! for every plan whose score is set to -∞ by the mask.

use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::outcome::ContractMaskHit;
use crate::combat::ai::pipeline::effects::{
    apply_score_effect_stage, EffectObservation, EmittedEffect, ScoreEffectStage, ScoreHit,
};
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::{MaskHit, MaskKind};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::pipeline::stages::sanity::plan_is_defensive;
use crate::combat::ai::scoring::factors::{PlanFactor, PlanFactorValues};

/// Mask non-defensive plans to `-∞` under `ProtectSelf` intent — contract
/// enforcement. A plan opt-out from the ProtectSelf contract is expressed
/// via `EvaluationMode != Default` (set upstream in `apply_adaptation`
/// when the contract is globally unsatisfiable → `ProtectSelfNoDefensive`
/// switches every plan's mode to `LastStand`). Plans in non-Default mode
/// are left alone by this mask.
///
/// Returns true if at least one plan was observed to be defensive. The
/// "no defensive plan at all" case is now handled by ADAPTATION one step
/// upstream — by the time this function runs, that case has already
/// switched all plans to `LastStand` mode, so every plan will skip the
/// mask. The return value is retained for callers that want to observe
/// contract satisfiability, but no longer triggers a LastStand rescore
/// inside this function.
pub(super) fn apply_protect_self_mask(
    scores: &mut [f32],
    raw: &[PlanFactorValues],
    modes: &[EvaluationMode],
    epsilon: f32,
) -> bool {
    debug_assert_eq!(raw.len(), modes.len());
    let mut any_defensive = false;
    for (i, f) in raw.iter().enumerate() {
        // Plans that adaptation moved to a non-Default mode have opted
        // out of the ProtectSelf contract; the mask does not apply to
        // them.
        if !matches!(modes.get(i), Some(EvaluationMode::Default)) {
            continue;
        }
        if plan_is_defensive(f.get_plan(PlanFactor::SelfSurvival), epsilon) {
            any_defensive = true;
        } else if i < scores.len() {
            scores[i] = f32::NEG_INFINITY;
        }
    }
    any_defensive
}

pub struct ProtectSelfMaskStage;

impl ScoreEffectStage for ProtectSelfMaskStage {
    fn id(&self) -> StageId {
        StageId::ProtectSelfMask
    }

    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect> {
        // Internal predicate — only active under ProtectSelf intent.
        if !matches!(ctx.intent, TacticalIntent::ProtectSelf) {
            return Vec::new();
        }

        let epsilon = ctx.scoring.world.tuning.thresholds.self_survival_epsilon;
        // Snapshot scores before masking.
        let pre_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Collect adaptation modes from annotations (needed by apply_protect_self_mask).
        let modes: Vec<_> = pool
            .annotations
            .iter()
            .map(|ann| {
                ann.adaptation
                    .as_ref()
                    .map(|_| EvaluationMode::LastStand)
                    .unwrap_or(EvaluationMode::Default)
            })
            .collect();

        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
        let mut scores: Vec<f32> = pre_scores.clone();
        apply_protect_self_mask(&mut scores, &raw_factors, &modes, epsilon);

        let mut emitted = Vec::new();
        for (plan_index, new_score) in scores.into_iter().enumerate() {
            if new_score == f32::NEG_INFINITY && pre_scores[plan_index].is_finite() {
                emitted.push(EmittedEffect {
                    plan_index,
                    hit: ScoreHit::Mask(MaskHit {
                        kind: MaskKind::Poison,
                        source: "protect_self",
                    }),
                    observability: Some(EffectObservation::Contract(ContractMaskHit {
                        mask: "protect_self".into(),
                        original_score: pre_scores[plan_index],
                    })),
                });
            }
        }
        emitted
    }
}

impl PlanStage for ProtectSelfMaskStage {
    fn name(&self) -> &'static str {
        "protect_self_mask"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_score_effect_stage(self, pool, ctx);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::factors::{PlanFactor, PlanFactorValues};
    use crate::combat::ai::intent::TacticalIntent;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn pfv_survival(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set_plan(PlanFactor::SelfSurvival, v);
        f
    }

    // ── internal predicate ────────────────────────────────────────────────────

    #[test]
    fn protect_self_mask_skips_when_intent_not_protect_self() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        // Reposition intent → stage is a no-op; no annotation, no score change.
        let h = StageTestHarness::new(actor);
        // intent is Reposition by default

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![pfv_survival(0.0)])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        assert_eq!(pool.annotations[0].score, 0.5, "score should be untouched for non-ProtectSelf intent");
        assert!(pool.annotations[0].contract().is_none(), "no contract annotation expected");
    }

    // ── mask writes contract annotation ───────────────────────────────────────

    #[test]
    fn protect_self_mask_writes_contract_when_non_defensive() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        // Two plans: one defensive (self_survival ≥ epsilon=0.01), one not.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.7])
            .factors(vec![pfv_survival(0.5), pfv_survival(0.0)]) // defensive, non-defensive
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        // plan 0: defensive → score unchanged, no annotation
        assert!(pool.annotations[0].score.is_finite(), "defensive plan should not be masked");
        assert!(pool.annotations[0].contract().is_none(), "no contract annotation for defensive plan");

        // plan 1: non-defensive → masked + annotation
        assert_eq!(pool.annotations[1].score, f32::NEG_INFINITY, "non-defensive plan should be masked");
        let contract = pool.annotations[1].contract()
            .expect("expected contract annotation for non-defensive plan");
        assert_eq!(contract.mask, "protect_self".to_string());
        assert_eq!(contract.original_score, 0.7_f32);
    }

    #[test]
    fn protect_self_mask_no_annotation_when_all_defensive() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.4])
            .factors(vec![pfv_survival(0.5), pfv_survival(0.3)])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        for ann in &pool.annotations {
            assert!(ann.contract().is_none(), "no contract annotation when all plans are defensive");
        }
    }

    // ── P3a.4: ScoreTrace emission ────────────────────────────────────────────

    #[test]
    fn p3a_protect_self_mask_emits_mask_hit() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool — survival=0.0 → non-defensive ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![pfv_survival(0.0)])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let trace = &pool.annotations[0].score_trace;
        assert_eq!(trace.masks.len(), 1, "exactly one MaskHit expected");
        assert_eq!(trace.masks[0].kind, crate::combat::ai::pipeline::score_trace::MaskKind::Poison);
        assert_eq!(trace.masks[0].source, "protect_self");
        assert!(trace.gates.is_empty(), "no GateHit expected for ProtectSelf mask");
    }

    #[test]
    fn p3a_protect_self_mask_no_hit_when_defensive() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool — survival=0.5 → defensive ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5])
            .factors(vec![pfv_survival(0.5)])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert!(ann.score.is_finite() && (ann.score - 0.5).abs() < 1e-6,
            "defensive plan score should be unchanged: got {}", ann.score);
        assert!(ann.score_trace.masks.is_empty(), "no MaskHit expected for defensive plan");
    }

    #[test]
    fn p3a_protect_self_mask_invariant() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.intent = TacticalIntent::ProtectSelf;

        // ── 3. Pool — non-defensive → will be masked ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.7])
            .factors(vec![pfv_survival(0.0)])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| ProtectSelfMaskStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert_eq!(ann.score, f32::NEG_INFINITY, "masked plan score must be NEG_INFINITY");
        assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY,
            "trace.compute() must equal NEG_INFINITY for masked plan");
    }
}
