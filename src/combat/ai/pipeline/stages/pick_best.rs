//! PickBestStage — step 7.4.
//!
//! Selects the winning plan from the scored pool using the same mercy + top-K
//! window logic that `PlanRanking::pick` used (via `pick_best_plan`). Writes
//! `annotation.chosen = true` and `annotation.pick = Some(PickInfo { .. })`
//! on the winning plan.

use crate::combat::ai::outcome::PickInfo;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::pick_best_plan;
use crate::combat::ai::planning::types::TurnPlan;
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use std::hash::{Hash, Hasher};

pub struct PickBestStage;

impl PlanStage for PickBestStage {
    fn name(&self) -> &'static str {
        "pick_best"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        if pool.is_empty() {
            return;
        }

        let scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let (best_idx, mech) = pick_best_plan(&scores, &raw_factors, ctx.scoring.world, ctx.rng);

        pool.annotations[best_idx].chosen = true;
        pool.annotations[best_idx].pick = Some(PickInfo { mechanics: mech, noise_applied: 0.0 });
    }
}

// ── Picking jitter ────────────────────────────────────────────────────────────

/// Apply deterministic, batch-scaled noise to every finite-score plan in the
/// pool. Mirrors Pass 2 from `scorer.rs::finalize_scores`; will replace it in
/// commit 2.
///
/// Returns a `Vec<f32>` (length `pool.len()`) with the accumulated noise
/// per plan (0.0 for skipped / masked plans). Mutates `pool.annotations[i].score`
/// in-place for finite scores.
///
/// **Spec semantics (8.C):** if `s_min` or `s_max` is not finite (all plans
/// masked), returns a zero vec immediately — no fallback to a constant spread.
/// This differs from the legacy `scorer.rs` which fell back to `spread = 0.05`.
///
/// NOT called from `PickBestStage::apply` until commit 2.
#[allow(dead_code)]
fn apply_pick_jitter(pool: &mut ScoredPool, ctx: &StageCtx) -> Vec<f32> {
    let noise_amp = ctx.scoring.world.difficulty.score_noise();
    let n = pool.len();
    let mut noise_per_plan = vec![0.0_f32; n];

    if noise_amp <= 0.0 || n == 0 {
        return noise_per_plan;
    }

    let (s_min, s_max) = pool
        .annotations
        .iter()
        .map(|a| a.score)
        .filter(|s| s.is_finite())
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s), hi.max(s))
        });

    // Spec semantics: early return if no finite scores (all masked).
    if !s_min.is_finite() || !s_max.is_finite() {
        return noise_per_plan;
    }

    let spread = (s_max - s_min).max(0.05);
    let effective_amp = noise_amp * spread;

    let actor = ctx.scoring.active.entity;
    let round = ctx.scoring.snap.round;

    for (i, (plan, ann)) in pool
        .plans
        .iter()
        .zip(pool.annotations.iter_mut())
        .enumerate()
    {
        if !ann.score.is_finite() {
            continue;
        }
        let n = plan_noise_internal(plan, round, actor, effective_amp);
        ann.score += n;
        noise_per_plan[i] = n;
    }

    noise_per_plan
}

/// Deterministic per-plan noise ∈ [−amp, +amp). Seed = hash((round, actor,
/// plan canonical key)) — order-invariant across any permutation of the plan
/// pool. Byte-for-byte copy of `scorer.rs::plan_noise`; will be deduplicated
/// in 8.C commit 2.
#[allow(dead_code)]
fn plan_noise_internal(plan: &TurnPlan, round: u32, actor: Entity, amp: f32) -> f32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    round.hash(&mut h);
    actor.hash(&mut h);
    plan.hash_canonical(plan_start_tile(plan), &mut h);
    let bits = h.finish();
    let u = ((bits >> 40) as u32) as f32 / (1u32 << 24) as f32;
    (u * 2.0 - 1.0) * amp
}

/// Returns a stable start tile for plan canonical hashing.
/// Byte-for-byte copy of `scorer.rs::plan_start_tile`.
/// // Copy of scorer.rs::plan_start_tile — будет удалён в 8.C commit 2.
#[allow(dead_code)]
fn plan_start_tile(plan: &TurnPlan) -> Hex {
    plan.final_pos
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::factors::PlanFactorValues;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    fn run_pick(scores: Vec<f32>) -> ScoredPool {
        let n = scores.len();
        let plans = vec![TurnPlan::default(); n];
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
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
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip(scores.into_iter()) {
            ann.score = score;
            ann.factors = PlanFactorValues::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);
        pool
    }

    #[test]
    fn pick_best_marks_exactly_one_chosen() {
        let pool = run_pick(vec![0.3, 0.8, 0.5]);
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");
    }

    #[test]
    fn pick_best_selects_highest_score() {
        // With deterministic DiceRng seed and no mercy margin (default difficulty),
        // the highest-scored plan should be chosen.
        let pool = run_pick(vec![0.1, 0.9, 0.4]);
        // Index 1 has the highest score.
        assert!(pool.annotations[1].chosen, "highest-scored plan should be chosen");
        assert!(pool.annotations[1].pick.is_some(), "chosen plan should have PickInfo");
    }

    #[test]
    fn pick_best_noop_on_empty_pool() {
        let pool = run_pick(vec![]);
        assert_eq!(pool.len(), 0);
    }

    // ── apply_pick_jitter tests ───────────────────────────────────────────────

    /// Build a pool with given scores and run apply_pick_jitter.
    /// Returns (noise_vec, pool) where pool.annotations[i].score is post-jitter.
    fn run_jitter(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        difficulty: &DifficultyProfile,
    ) -> (Vec<f32>, Vec<f32>) {
        assert_eq!(plans.len(), scores.len());
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, s) in pool.annotations.iter_mut().zip(scores.iter()) {
            ann.score = *s;
        }
        let noise = apply_pick_jitter(&mut pool, &ctx);
        let post_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        (noise, post_scores)
    }

    /// score_noise = 0.0 (normal difficulty) → jitter returns all-zeros, scores unchanged.
    #[test]
    fn pick_jitter_no_op_when_noise_amp_zero() {
        let difficulty = DifficultyProfile::normal();
        assert_eq!(difficulty.score_noise(), 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        let scores = vec![0.1_f32, 0.5, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores.clone(), &difficulty);

        assert_eq!(noise, vec![0.0_f32; 3], "noise vec must be all zeros");
        assert_eq!(post_scores, scores, "scores must be unchanged");
    }

    /// Plans with score = -inf (masked) must have noise[i] = 0.0 and score unchanged.
    #[test]
    fn pick_jitter_skips_masked_plans() {
        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        // Middle plan is masked.
        let scores = vec![0.5_f32, f32::NEG_INFINITY, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores, &difficulty);

        assert_eq!(noise[1], 0.0, "masked plan noise must be zero");
        assert_eq!(post_scores[1], f32::NEG_INFINITY, "masked plan score must be unchanged");
        // Non-masked plans get non-zero noise (deterministic, may be any value).
        // Just verify they're finite.
        assert!(post_scores[0].is_finite(), "plan 0 score should be finite");
        assert!(post_scores[2].is_finite(), "plan 2 score should be finite");
    }

    /// Noise is order-invariant: same plan in position 0 or 1 gets the same noise value.
    /// Migrates the invariant tested in `scorer.rs::noise_is_plan_order_invariant`.
    #[test]
    fn pick_jitter_is_plan_order_invariant() {
        use crate::combat::ai::planning::types::{PlanStep, StepOutcome};

        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let pos_a = hex_from_offset(3, 0);
        let pos_b = hex_from_offset(2, 0);

        // Two distinct plans targeting different positions (different canonical hash).
        let mk_plan = |target_pos: crate::game::hex::Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: bevy::prelude::Entity::from_raw_u32(99).expect("valid"),
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let plan_a = mk_plan(pos_a);
        let plan_b = mk_plan(pos_b);

        let scores = vec![0.5_f32, 0.5];

        // Order AB.
        let (noise_ab, _) = run_jitter(vec![plan_a.clone(), plan_b.clone()], scores.clone(), &difficulty);
        // Order BA.
        let (noise_ba, _) = run_jitter(vec![plan_b.clone(), plan_a.clone()], scores.clone(), &difficulty);

        // noise_ab[0] = noise for plan_a; noise_ba[1] = noise for plan_a.
        assert_eq!(
            noise_ab[0], noise_ba[1],
            "plan_a noise must not depend on pool position",
        );
        assert_eq!(
            noise_ab[1], noise_ba[0],
            "plan_b noise must not depend on pool position",
        );
    }
}
