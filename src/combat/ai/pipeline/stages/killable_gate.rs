//! KillableGateStage — step 7.2.
//!
//! Replicates `PlanRanking::apply_killable_gate` as a `PlanStage` with an
//! **internal predicate**: the stage skips entirely when `ctx.intent` is not
//! `FocusTarget { .. }`. This removes the corresponding `if matches!` guard
//! from `pick_action` body.
//!
//! Writes `annotation.contract = Some(ContractMaskHit { mask: "killable_gate", … })`
//! for every plan whose score is set to -∞ by the gate.

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::outcome::ContractMaskHit;
use crate::combat::ai::pipeline::score_trace::{GateHit, GateOutcome, MaskHit, MaskKind};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::apply_killable_gate;

pub struct KillableGateStage;

impl PlanStage for KillableGateStage {
    fn name(&self) -> &'static str {
        "killable_gate"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Internal predicate — only active under FocusTarget intent.
        if !matches!(ctx.intent, TacticalIntent::FocusTarget { .. }) {
            return;
        }

        // Snapshot scores before the gate mutates them.
        let pre_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Build modes slice from annotations (same logic as ProtectSelfMaskStage).
        let modes: Vec<_> = pool
            .annotations
            .iter()
            .map(|ann| {
                ann.adaptation
                    .as_ref()
                    .map(|_| crate::combat::ai::adapt::EvaluationMode::LastStand)
                    .unwrap_or(crate::combat::ai::adapt::EvaluationMode::Default)
            })
            .collect();

        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
        let mut scores: Vec<f32> = pre_scores.clone();
        apply_killable_gate(
            &pool.plans,
            &raw_factors,
            &mut scores,
            &modes,
            &ctx.intent,
            ctx.scoring.snap,
        );

        // Write back updated scores and contract annotations.
        for (i, (ann, new_score)) in pool.annotations.iter_mut().zip(scores.into_iter()).enumerate() {
            if new_score == f32::NEG_INFINITY && pre_scores[i].is_finite() {
                ann.contract = Some(ContractMaskHit {
                    mask: "killable_gate".into(),
                    original_score: pre_scores[i],
                });

                // P3a.4 / P3a.6: double-emit on the accumulated trace —
                // GateHit (PostScoreGate classification) + MaskHit Poison
                // (maintains compute() == NEG_INFINITY while KillableGate still
                // sets NEG_INFINITY). Bridging-reset removed.
                ann.score_trace.push_gate(GateHit {
                    outcome: GateOutcome::Reject,
                    source: "killable_gate",
                });
                ann.score_trace.push_mask(MaskHit {
                    kind: MaskKind::Poison,
                    source: "killable_gate",
                });

                // Invariant: for gated plans, ann.score == compute() == NEG_INFINITY.
                debug_assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY);
            }
            ann.score = new_score;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::{PlanFactorValues, StepFactor};
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder, ent,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::{AbilityId, DiceRng};

    fn run_stage(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<PlanFactorValues>,
        intent: TacticalIntent,
        snap: &BattleSnapshot,
        actor: &crate::combat::ai::world::snapshot::UnitSnapshot,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, snap, &maps, &reservations, actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            intent,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, (score, raw_f)) in pool.annotations.iter_mut().zip(scores.into_iter().zip(raw.into_iter())) {
            ann.score = score;
            ann.factors = raw_f;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            if score.is_finite() {
                ann.score_trace.base = score;
            }
        }
        KillableGateStage.apply(&mut pool, &mut ctx);
        pool
    }

    fn pfv_kill_now(v: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set(StepFactor::KillNow, v);
        f
    }

    // ── internal predicate ────────────────────────────────────────────────────

    #[test]
    fn killable_gate_skips_when_intent_not_focus_target() {
        // Reposition intent → stage is a no-op; scores unchanged, no annotation.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);

        let plans = vec![TurnPlan::default()];
        let scores = vec![0.5_f32];
        let raw = vec![PlanFactorValues::default()];

        let pool = run_stage(plans, scores, raw, TacticalIntent::Reposition, &snap, &actor);

        assert_eq!(pool.annotations[0].score, 0.5, "score should be untouched for non-FocusTarget intent");
        assert!(pool.annotations[0].contract.is_none(), "no contract annotation expected");
    }

    // ── gate writes annotation when pruning ───────────────────────────────────

    #[test]
    fn killable_gate_writes_contract_when_active() {
        // FocusTarget with a CanFinish plan (kill_now=1.0) and a non-offensive plan.
        // The gate should prune the non-offensive plan and write the annotation.
        let pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = ent(2);

        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(1).max_hp(10).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);

        // Plan 0: offensive vs target with kill_now=1.0 (can finish).
        let offensive_plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("attack"),
                target: target_entity,
                target_pos,
            }],
            final_pos: pos,
            ..TurnPlan::default()
        };
        // Plan 1: move (non-offensive vs target).
        let non_offensive_plan = TurnPlan {
            steps: vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }],
            final_pos: hex_from_offset(1, 0),
            ..TurnPlan::default()
        };

        let plans = vec![offensive_plan, non_offensive_plan];
        let scores = vec![0.5_f32, 0.6_f32];
        let raw = vec![
            pfv_kill_now(1.0),       // CanFinish
            PlanFactorValues::default(), // no kill signal
        ];

        let pool = run_stage(
            plans, scores, raw,
            TacticalIntent::FocusTarget { target: target_entity },
            &snap, &actor,
        );

        // plan 1 should be masked and annotated
        assert_eq!(pool.annotations[1].score, f32::NEG_INFINITY, "non-offensive plan should be gated");
        let contract = pool.annotations[1].contract.as_ref()
            .expect("expected contract annotation for gated plan");
        assert_eq!(contract.mask, "killable_gate".to_string());
        assert_eq!(contract.original_score, 0.6_f32);

        // plan 0 should be untouched
        assert!(pool.annotations[0].score.is_finite(), "offensive plan should survive gate");
        assert!(pool.annotations[0].contract.is_none(), "no contract annotation for offensive plan");
    }

    #[test]
    fn killable_gate_no_annotation_when_gate_does_not_fire() {
        // FocusTarget but no kill signal → gate returns early, no annotations.
        let pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = ent(2);

        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(10).max_hp(10).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);

        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let scores = vec![0.5_f32, 0.4_f32];
        // No kill_now, no pressure-level damage → gate returns early.
        let raw = vec![PlanFactorValues::default(), PlanFactorValues::default()];

        let pool = run_stage(
            plans, scores, raw,
            TacticalIntent::FocusTarget { target: target_entity },
            &snap, &actor,
        );

        for ann in &pool.annotations {
            assert!(ann.contract.is_none(), "no contract annotation when gate does not fire");
        }
    }

    // ── P3a.4: ScoreTrace emission ────────────────────────────────────────────

    #[test]
    fn p3a_killable_gate_emits_gate_and_mask_hits() {
        // Non-offensive plan under FocusTarget with kill signal → GateHit + MaskHit Poison.
        let pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = ent(2);

        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(1).max_hp(10).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);

        let offensive_plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("attack"),
                target: target_entity,
                target_pos,
            }],
            final_pos: pos,
            ..TurnPlan::default()
        };
        let non_offensive_plan = TurnPlan {
            steps: vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }],
            final_pos: hex_from_offset(1, 0),
            ..TurnPlan::default()
        };

        let plans = vec![offensive_plan, non_offensive_plan];
        let scores = vec![0.5_f32, 0.6_f32];
        let raw = vec![
            pfv_kill_now(1.0),
            PlanFactorValues::default(),
        ];

        let pool = run_stage(
            plans, scores, raw,
            TacticalIntent::FocusTarget { target: target_entity },
            &snap, &actor,
        );

        let trace = &pool.annotations[1].score_trace;
        assert_eq!(trace.gates.len(), 1, "exactly one GateHit expected");
        assert_eq!(trace.gates[0].outcome, crate::combat::ai::pipeline::score_trace::GateOutcome::Reject);
        assert_eq!(trace.gates[0].source, "killable_gate");
        assert_eq!(trace.masks.len(), 1, "exactly one MaskHit Poison expected");
        assert_eq!(trace.masks[0].kind, crate::combat::ai::pipeline::score_trace::MaskKind::Poison);
        assert!(trace.is_gated(), "is_gated() must return true for gated plan");
    }

    #[test]
    fn p3a_killable_gate_no_hit_when_intent_not_focus_target() {
        // Reposition intent → stage skips entirely; trace.gates and trace.masks empty.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);

        let plans = vec![TurnPlan::default()];
        let scores = vec![0.5_f32];
        let raw = vec![pfv_kill_now(1.0)];

        let pool = run_stage(plans, scores, raw, TacticalIntent::Reposition, &snap, &actor);

        let trace = &pool.annotations[0].score_trace;
        assert!(trace.gates.is_empty(), "no GateHit for non-FocusTarget intent");
        assert!(trace.masks.is_empty(), "no MaskHit for non-FocusTarget intent");
    }

    #[test]
    fn p3a_killable_gate_compute_returns_neg_infinity() {
        // Gated plan: trace.compute() == NEG_INFINITY (due to Poison mask).
        let pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = ent(2);

        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(1).max_hp(10).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);

        let offensive_plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("attack"),
                target: target_entity,
                target_pos,
            }],
            final_pos: pos,
            ..TurnPlan::default()
        };
        let non_offensive_plan = TurnPlan {
            steps: vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }],
            final_pos: hex_from_offset(1, 0),
            ..TurnPlan::default()
        };

        let plans = vec![offensive_plan, non_offensive_plan];
        let scores = vec![0.5_f32, 0.6_f32];
        let raw = vec![pfv_kill_now(1.0), PlanFactorValues::default()];

        let pool = run_stage(
            plans, scores, raw,
            TacticalIntent::FocusTarget { target: target_entity },
            &snap, &actor,
        );

        assert_eq!(
            pool.annotations[1].score_trace.compute(),
            f32::NEG_INFINITY,
            "trace.compute() must be NEG_INFINITY for gated plan"
        );
    }
}
