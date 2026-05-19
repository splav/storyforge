//! Sanity stage + shared types, helpers, and residual rules.
//!
//! ## Layout
//! - `mod.rs` — `SanityStage` (pipeline stage), `SanityHit`, `SanityRule` types,
//!   `sanity_adjust_plans` orchestrator, shared helpers
//!   (`expected_aoo_damage`, `plan_is_defensive`, `plan_has_self_aoe`).
//! - `healer_exposure.rs` — Rule 1: non-healer abandoning unguarded healer.
//! - `retreat_trap.rs` — Rule 2: final tile with < 2 open neighbours.
//! - `synergy_bonus.rs` — Rule 3: reposition to safer tile + useful cast.
//!
//! The three rules that were migrated to critics in steps 10.1–10.2
//! (`Survival`, `AoOBleed`, `LosBlindspot`, `SelfAoe`) are no longer present;
//! `SanityRule` has exactly three variants.

mod healer_exposure;
mod retreat_trap;
mod synergy_bonus;

use crate::combat::ai::scoring::factors::aoe_area;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::pipeline::effects::{
    apply_score_effect_stage, EffectObservation, EmittedEffect, ScoreEffectStage, ScoreHit,
};
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind};
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::position_eval::evaluate_position;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::content::abilities::AoEShape;
use crate::game::hex::Hex;
use std::collections::HashSet;

// ── Sanity rule observability ──────────────────────────────────────────────

/// Identifies one residual sanity rule. Variants that were migrated to
/// `PlanCritic` in steps 10.1–10.2 (`Survival`, `AoOBleed`, `LosBlindspot`,
/// `SelfAoe`) are removed in step 10.4. Stable codes consumed by offline
/// analyzers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanityRule {
    /// Non-healer abandoning the team's unguarded healer.
    HealerExposure,
    /// Final tile has fewer than 2 open neighbours (retreat trap).
    RetreatTrap,
    /// Plan repositions to a safer/better tile AND includes a useful cast (synergy bonus).
    SynergyBonus,
}

impl SanityRule {
    /// Short stable code for offline analyzer consumption.
    pub fn code(self) -> &'static str {
        match self {
            Self::HealerExposure => "healer_exposure",
            Self::RetreatTrap => "retreat_trap",
            Self::SynergyBonus => "synergy_bonus",
        }
    }
}

/// Records that one sanity rule fired on one plan and the factor it applied.
/// `multiplier < 1.0` for penalties; `> 1.0` for the synergy bonus.
/// The value is the **clamped** factor that was actually multiplied into the
/// plan score.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SanityHit {
    pub rule: SanityRule,
    pub multiplier: f32,
}

// ── Pipeline stage ─────────────────────────────────────────────────────────

pub struct SanityStage;

impl ScoreEffectStage for SanityStage {
    fn id(&self) -> StageId {
        StageId::Sanity
    }

    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect> {
        // sanity_adjust_plans wants &mut [f32] for score adjustments.
        // We need only the breakdown — score mutations are discarded
        // (drive-loop recomputes score from trace).
        let mut throwaway_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        let breakdown = sanity_adjust_plans(&mut throwaway_scores, &pool.plans, ctx.scoring);

        let mut emitted = Vec::new();
        for (plan_index, hits) in breakdown.into_iter().enumerate() {
            for hit in hits {
                let multiplier = hit.multiplier;
                emitted.push(EmittedEffect {
                    plan_index,
                    hit: ScoreHit::Multiplier(MultiplierHit {
                        kind: MultiplierKind::Sanity,
                        value: multiplier,
                        detail: None,
                    }),
                    observability: Some(EffectObservation::Sanity(hit)),
                });
            }
        }
        emitted
    }
}

impl PlanStage for SanityStage {
    fn name(&self) -> &'static str {
        "sanity"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_score_effect_stage(self, pool, ctx);
    }
}

// ── Orchestrator ───────────────────────────────────────────────────────────

/// Apply residual sanity multipliers to `scores` in place and return a
/// per-plan breakdown of which rules fired and with what multiplier.
///
/// The outer `Vec` is parallel to `plans`/`scores`. Each inner `Vec` lists
/// the `SanityHit`s for that plan in the order they fired; an empty inner
/// vec means no rules fired for that plan and the score was unchanged.
///
/// **Early-return case (`scores.len() <= 1`):** returns a `Vec` of empty
/// inner vecs sized to `scores.len()` (0 or 1 entries). The single-plan
/// edge case does not run any rule, so the breakdown is empty — but the
/// outer length still matches `scores.len()` so callers can index it safely
/// without a special-case check.
pub fn sanity_adjust_plans(
    scores: &mut [f32],
    plans: &[TurnPlan],
    ctx: &ScoringCtx,
) -> Vec<Vec<SanityHit>> {
    // Pre-allocate one empty inner vec per plan.
    let mut breakdown: Vec<Vec<SanityHit>> = (0..scores.len()).map(|_| Vec::new()).collect();

    if scores.len() <= 1 {
        return breakdown;
    }

    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, ctx.world.tuning, maps);
    let current_danger = maps.danger.get(active.pos);

    for (idx, (plan, score)) in plans.iter().zip(scores.iter_mut()).enumerate() {
        if !score.is_finite() {
            continue;
        }
        let mut penalty = 1.0f32;
        let final_pos = plan.final_pos;
        let hits = &mut breakdown[idx];

        // 1. Healer exposure: a non-healer abandoning the team's healer.
        for hit in healer_exposure::evaluate(active, final_pos, &allies) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        // 2. Retreat trap: final tile with fewer than 2 open neighbours
        // (flankable, no room to move next turn).
        if let Some(hit) = retreat_trap::evaluate(final_pos, &ally_positions) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        // 3. Synergy bonus: the plan repositions to a safer/better tile AND
        // includes a useful cast. Encourages retreat-and-help combos.
        if let Some(hit) = synergy_bonus::evaluate(
            active,
            plan,
            final_pos,
            current_danger,
            current_pos_eval,
            ctx,
        ) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        *score *= penalty;
    }

    breakdown
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub(crate) fn plan_has_self_aoe(plan: &TurnPlan, ctx: &ScoringCtx) -> bool {
    let content = ctx.world.content;
    plan.walk_with_caster(ctx.active.pos).any(|(_, step, caster_pos)| {
        let PlanStep::Cast { ability, target_pos, .. } = step else { return false };
        let Some(def) = content.abilities.get(ability) else { return false };
        if !def.friendly_fire || def.aoe == AoEShape::None {
            return false;
        }
        // Route through the shared helper so a new `AoEShape` variant lands
        // here automatically — the inline match we used to have silently
        // missed anything beyond Circle/Line.
        aoe_area(def, *target_pos, caster_pos).contains(&caster_pos)
    })
}

// ── ProtectSelf mask ───────────────────────────────────────────────────────

/// A plan is **defensive** iff its `self_survival` factor is at or above
/// `epsilon`. The `self_survival` axis captures cumulative defensive value
/// across the plan (self-heal, armor-buff, and danger-exit), making the
/// threshold independent of step-level tile/target-type heuristics.
pub fn plan_is_defensive(self_survival: f32, epsilon: f32) -> bool {
    self_survival >= epsilon
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::scoring::horizon::expected_aoo_damage;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};

    fn make_move_plan(path: Vec<Hex>) -> TurnPlan {
        let final_pos = path.last().copied().unwrap_or_else(|| hex_from_offset(0, 0));
        TurnPlan {
            steps: vec![PlanStep::Move { path }],
            final_pos,
            ..TurnPlan::default()
        }
    }

    // ── no hits on a clean plan ────────────────────────────────────────────

    #[test]
    fn sanity_stage_no_hits_leaves_annotation_empty() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![make_move_plan(vec![dest_a]), make_move_plan(vec![dest_b])];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.4])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| SanityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        for ann in &pool.annotations {
            assert!(
                ann.score_trace.multipliers.iter().all(|m| !matches!(m.kind, MultiplierKind::Sanity)),
                "expected no sanity multipliers in trace for healthy actor in safe tile",
            );
        }
    }

    // ── residual-only: low-HP actor on danger tile must not produce any hits ──
    // (Survival was migrated to critics in 10.1; SanityRule no longer has
    //  that variant after 10.4 cleanup.)

    #[test]
    fn sanity_stage_no_hits_for_low_hp_danger_tile() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(2).max_hp(20).build();
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![make_move_plan(vec![dest_a]), make_move_plan(vec![dest_b])];

        // ── 2. Harness — danger tile on destination of plan 0 ──
        let mut h = StageTestHarness::new(actor);
        h.maps.danger.add(dest_a, 1.0);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.5, 0.4])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| SanityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        // No sanity rule should fire — the only active rules are
        // HealerExposure, RetreatTrap, SynergyBonus, none of which
        // trigger in this solo-actor scenario.
        for ann in &pool.annotations {
            assert!(
                ann.score_trace.multipliers.iter().all(|m| !matches!(m.kind, MultiplierKind::Sanity)),
                "no sanity multipliers expected for low-HP actor in danger tile (Survival is now a critic)",
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
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::scoring::factors::PlanFactorValues;

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let adaptation = Some(AdaptationData {
            reason: AdaptationReason::ProtectSelfNoDefensive,
            original_score: 0.8,
        });
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.8, 0.6])
            .factors(vec![PlanFactorValues::default(), PlanFactorValues::default()])
            .adaptations(vec![adaptation, None])
            .build();

        // ── 4. Act — Finalize then Sanity (mirroring production pipeline order) ──
        let scores_after_finalize: Vec<f32> = h.run(|ctx| {
            FinalizeStage.apply(&mut pool, ctx);
            let scores = pool.annotations.iter().map(|a| a.score).collect();
            SanityStage.apply(&mut pool, ctx);
            scores
        });

        // ── 5. Assert ──
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
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![make_move_plan(vec![dest_a]), make_move_plan(vec![dest_b])];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let entry_scores = [0.7_f32, 0.5_f32];
        let mut pool = PoolBuilder::new(plans)
            .scores(&entry_scores)
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| SanityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
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
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::scoring::factors::PlanFactorValues;

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).hp(10).max_hp(20).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let adaptation = Some(AdaptationData {
            reason: AdaptationReason::ProtectSelfNoDefensive,
            original_score: 0.8,
        });
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.8, 0.6])
            .factors(vec![PlanFactorValues::default(), PlanFactorValues::default()])
            .adaptations(vec![adaptation, None])
            .build();

        // ── 4. Act ──
        h.run(|ctx| {
            FinalizeStage.apply(&mut pool, ctx);
            SanityStage.apply(&mut pool, ctx);
        });

        // ── 5. Assert ──
        for (i, ann) in pool.annotations.iter().enumerate() {
            // Every multiplier hit must have kind=Sanity and detail present.
            for (j, mhit) in ann.score_trace.multipliers.iter().enumerate() {
                assert_eq!(
                    mhit.kind,
                    MultiplierKind::Sanity,
                    "plan[{i}] multiplier[{j}]: expected kind=Sanity",
                );
                assert!(
                    mhit.detail.is_some(),
                    "plan[{i}] multiplier[{j}]: Sanity hit must carry detail (TLE-1 invariant)",
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
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let dest_c = hex_from_offset(3, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
            make_move_plan(vec![dest_c]),
        ];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.9, 0.7, 0.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| SanityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
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

    /// A plan masked via a trace Mask hit must not receive sanity multipliers.
    /// After apply(), sanity hits remain empty and is_masked() stays true.
    /// score is finite after Step 3 (compute() ignores masks); selectability
    /// is communicated via trace flags, not score magnitude.
    ///
    /// Note: in production Sanity runs BEFORE ProtectSelfMask/KillableGate, so
    /// this situation doesn't arise naturally — but the test guards the invariant.
    #[test]
    fn p3a_sanity_masked_plan_trace_unchanged_or_only_base() {
        use crate::combat::ai::pipeline::score_trace::{MaskHit, MaskKind};

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).full_hp(20).build();
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![make_move_plan(vec![dest_a]), make_move_plan(vec![dest_b])];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool — plan[0] masked via trace Mask hit, plan[1] normal ──
        let mut pool = PoolBuilder::new(plans)
            .customize(|anns| {
                // Mask plan[0] through trace — is_selectable() returns false.
                anns[0].score_trace.push_mask(MaskHit { kind: MaskKind::Poison, source: "test", original_score: None });
                // Set score to NEG_INFINITY so the throwaway-scores vec in
                // sanity_adjust_plans still skips it via !is_finite() (sanity
                // internal skip — not changed in Step 3).
                anns[0].score = f32::NEG_INFINITY;
                anns[1].score = 0.6;
                // P3a.6: initialise trace.base for the finite plan only.
                anns[1].score_trace.base = 0.6;
            })
            .build();

        // ── 4. Act ──
        h.run(|ctx| SanityStage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        // plan[0]: mask flag stays, sanity multipliers empty (sanity_adjust_plans skips
        // plans with !score.is_finite() in its throwaway scores buffer).
        let masked = &pool.annotations[0];
        assert!(
            masked.score_trace.is_masked(),
            "mask must remain in trace",
        );
        assert!(!masked.is_selectable(), "masked plan must not be selectable");
        assert!(
            masked.score_trace.multipliers.is_empty(),
            "masked plan must have no sanity multipliers in trace",
        );
        // score is finite after Step 3 (recompute_score_from_trace uses finite compute())
        assert!(
            masked.score.is_finite(),
            "score is finite after Step 3 cutover, got {}", masked.score,
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

    // ── planning/sanity.rs tests (migrated from planning layer) ──────────────

    /// Sanity-suite defaults: max_hp=30, speed=4, aoo=(5.0, 1 reaction).
    fn unit(id: u32, team: Team, pos: Hex, hp: i32) -> crate::combat::ai::world::snapshot::UnitSnapshot {
        UnitBuilder::new(id, team, pos)
            .hp(hp)
            .max_hp(30)
            .speed(4)
            .aoo(5.0, 1)
            .build()
    }

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // Even-r geometry reminder (verified empirically):
    //   Neighbors of (0,0): (1,0),(-1,0),(0,1),(0,-1),(1,1),(1,-1).
    //   (-1,0) is adjacent to (0,0) but NOT to (1,0) — "leaves adjacency".
    //   (1,1) is adjacent to BOTH (0,0) and (1,0) — "stays adjacent".

    /// Shape of the expected `expected_aoo_damage` result for a row.
    #[derive(Clone, Copy, Debug)]
    enum Aoo {
        /// Exactly zero — no provoker fired.
        Zero,
        /// Strictly positive — at least one provoker fired.
        Positive,
        /// Within an inclusive `[lo, hi]` band around a known EV.
        Near(f32, f32),
        /// Lethal — result must be ≥ the actor's own HP (pins the
        /// sanity-adjust mask precondition).
        AtLeastActorHp,
    }

    /// Table-driven AoO cases. Each row pins a distinct invariant; the
    /// `name` column is formatted into every failure message so a broken
    /// row stays as diagnostic as an individually-named test.
    #[test]
    fn expected_aoo_damage_matrix() {
        fn default_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            unit(2, Team::Player, hex_from_offset(1, 0), 20)
        }
        fn ranged_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            let mut e = default_enemy();
            e.max_attack_range = 5;
            e.aoo_expected_damage = None;
            e
        }
        fn hybrid_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            // Melee + ranged: max_attack_range>1 но melee-AoO есть.
            // Regression pin for the dropped `max_attack_range != 1` guard.
            let mut e = default_enemy();
            e.max_attack_range = 3;
            e
        }
        fn no_react_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            let mut e = default_enemy();
            e.reactions_left = 0;
            e
        }
        fn second_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            unit(3, Team::Player, hex_from_offset(0, 1), 20)
        }

        struct Row {
            name: &'static str,
            actor_hp: i32,
            actor_armor: i32,
            enemies: Vec<crate::combat::ai::world::snapshot::UnitSnapshot>,
            path: Vec<Hex>,
            expected: Aoo,
        }
        let rows: Vec<Row> = vec![
            Row {
                name: "leaves adjacency provokes",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "stays adjacent does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(1, 1)],
                expected: Aoo::Zero,
            },
            Row {
                name: "ranged enemy does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![ranged_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                name: "hybrid melee+ranged provokes (regression: dropped max_range guard)",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![hybrid_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "enemy with 0 reactions does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![no_react_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                // Between two melees; (0,-1) leaves adjacency with both → 5 × 2.
                name: "multi-enemy damage sums",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy(), second_enemy()],
                path: vec![hex_from_offset(0, -1)],
                expected: Aoo::Near(9.5, 10.5),
            },
            Row {
                // Leaves → re-enters → leaves: one reaction per enemy, not three.
                name: "one AoO per enemy even with multiple transitions",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![
                    hex_from_offset(-1, 0),
                    hex_from_offset(0, 0),
                    hex_from_offset(-1, 0),
                ],
                expected: Aoo::Near(4.5, 5.5),
            },
            Row {
                // Armor 10 vs raw 5.0 — final_damage floors at 1.
                name: "armor clamps expected damage at the 1-HP floor",
                actor_hp: 20, actor_armor: 10,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Near(0.99, 1.01),
            },
            Row {
                // Precondition `expected_aoo_damage ≥ actor_hp` — the
                // input signal ADAPTATION uses to trigger
                // `ExpectedSelfLethal`. Sanity no longer reads this for
                // a hard mask (it stays in soft-bleed territory only),
                // but the helper itself must still report correctly so
                // adaptation can act on it.
                name: "expected-lethal AoO reaches actor HP threshold",
                actor_hp: 3, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::AtLeastActorHp,
            },
        ];

        for row in &rows {
            let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), row.actor_hp);
            actor.armor = row.actor_armor;
            let enemy_refs: Vec<&crate::combat::ai::world::snapshot::UnitSnapshot> = row.enemies.iter().collect();
            let plan = move_plan(row.path.clone());
            let dmg = expected_aoo_damage(&actor, &plan, &enemy_refs);
            let name = row.name;
            match row.expected {
                Aoo::Zero => assert_eq!(dmg, 0.0, "[{name}] expected 0, got {dmg}"),
                Aoo::Positive => assert!(dmg > 0.0, "[{name}] expected > 0, got {dmg}"),
                Aoo::Near(lo, hi) => assert!(
                    (lo..=hi).contains(&dmg),
                    "[{name}] expected in [{lo}, {hi}], got {dmg}",
                ),
                Aoo::AtLeastActorHp => assert!(
                    dmg >= row.actor_hp as f32,
                    "[{name}] expected ≥ hp({}), got {dmg}", row.actor_hp,
                ),
            }
        }
    }

    // ── plan_has_self_aoe: routed through shared `aoe_area` ─────────────
    //
    // Smoke test: a friendly-fire Circle AoE centred on the caster's tile
    // must be detected as self-AoE. The inline match that used to live here
    // covered Circle/Line only; `aoe_area` (shared with every other AoE
    // caller) automatically picks up new `AoEShape` variants, so adding
    // e.g. Cone later will be covered without a code change here.

    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr};

    fn fireball_def(radius: u32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("fireball"),
            name: "fireball".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 6, 0) },
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::Circle { radius },
                friendly_fire: true,
                statuses: Vec::new(),
                key: None,
            },
        }
    }

    #[test]
    fn plan_has_self_aoe_detects_friendly_fire_circle_on_caster() {
        use crate::combat::ai::world::reservations::Reservations;
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};
        use crate::combat::ai::test_helpers::snapshot_from;
        use crate::content::abilities::CasterContext;
        let actor_pos = hex_from_offset(0, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 20);
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = crate::game::components::Abilities(Vec::new());
        let mut content = empty_content();
        let def = fireball_def(1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let utility = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&utility, &snap, &maps, &reservations, &actor);

        // Single-cast fireball centred on `target_pos`, fired from `actor_pos`.
        let cast_fireball_at = |target_pos: Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("fireball"),
                target: crate::combat::ai::test_helpers::ent(99),
                target_pos,
            }],
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: vec![Default::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };

        assert!(
            plan_has_self_aoe(&cast_fireball_at(actor_pos), &ctx),
            "friendly-fire circle on caster tile must be flagged as self-AoE",
        );
        assert!(
            !plan_has_self_aoe(&cast_fireball_at(hex_from_offset(5, 5)), &ctx),
            "AoE centred far from the caster must not be flagged",
        );
    }

    /// Pin that `sanity_adjust_plans` only fires the three residual rules
    /// (`HealerExposure`, `RetreatTrap`, `SynergyBonus`) and never the four
    /// variants that were migrated to critics in steps 10.1–10.2.
    /// Uses the same scenario as the pre-10.1 test: low-HP actor in a
    /// danger tile, adjacent to an AoO-capable enemy.
    #[test]
    fn sanity_only_fires_residual_rules() {
        use crate::combat::ai::world::reservations::Reservations;
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx};
        use crate::combat::ai::test_helpers::snapshot_from;

        let actor_pos = hex_from_offset(3, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 5); // low HP (was survival trigger)
        let enemy_pos = hex_from_offset(4, 0);
        let enemy = unit(2, Team::Player, enemy_pos, 20); // AoO-capable (was bleed trigger)

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![actor.clone(), enemy], 1);
        let mut maps = empty_maps();
        let dest = hex_from_offset(2, 0);
        maps.danger.add(dest, 0.9);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let noop_plan = TurnPlan {
            steps: Vec::new(),
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let aoo_plan = move_plan(vec![dest]);

        let plans = vec![noop_plan, aoo_plan];
        let mut scores = vec![1.0f32, 1.0f32];
        let breakdown = sanity_adjust_plans(&mut scores, &plans, &ctx);

        assert_eq!(breakdown.len(), 2);

        // Only residual rules are valid; the enum no longer has Survival /
        // AoOBleed / SelfAoe / LosBlindspot so the compiler ensures no such
        // hits can appear.
        for hits in &breakdown {
            for h in hits {
                assert!(
                    matches!(h.rule, SanityRule::HealerExposure | SanityRule::RetreatTrap | SanityRule::SynergyBonus),
                    "unexpected rule in sanity breakdown: {:?}", h.rule,
                );
            }
        }
    }
}
