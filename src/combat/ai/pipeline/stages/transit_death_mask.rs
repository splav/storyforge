//! TransitDeathMaskStage — Fix C: veto lethal-AoO movement plans.
//!
//! A plan where the actor accumulates lethal self-damage (≥ actor HP) on a
//! **Move** step that precedes any terminal action accomplishes nothing: the
//! actor dies in transit before the terminal step can execute. This differs
//! from death-after-acting (where the terminal Cast fires first, then the
//! actor may die from retaliation) — LastStand heroic-trade applies to the
//! latter but NOT to transit death.
//!
//! This stage masks (`score = −∞`, `selectable = false`) such plans
//! unconditionally regardless of intent. The non-empty-candidate invariant
//! is guaranteed by `pick_best_plan`'s fallback path: if every candidate is
//! masked, it falls back to the highest-ranked masked plan (ranked[0]) rather
//! than panicking or returning an empty result.
//!
//! # Relationship to ModeSelectionStage
//!
//! `ModeSelectionStage` (which runs before us) explicitly SKIPS transit-death
//! plans when assigning `EvaluationMode::LastStand` for `ExpectedSelfLethal`.
//! We then mask them here. The two stages are complementary:
//! - ModeSelection: "transit-death plans do NOT get heroic LastStand mode"
//! - TransitDeathMask: "transit-death plans are masked from normal selection"
//!
//! # Detection
//!
//! Uses `plan_has_lethal_transit` from `adapt::select`, which reads
//! `plan.outcomes` (real per-step sim values, not the EV estimate). This is
//! critical: the `ActionOutcomeEstimate` builder zeroes `self_damage` for
//! Move arms, so the estimate channel is blind to move-induced death. The
//! `plan.outcomes` field carries the truth from `sim::apply_move`.

use crate::combat::ai::adapt::plan_has_lethal_transit;
use crate::combat::ai::outcome::ContractMaskHit;
use crate::combat::ai::pipeline::effects::{
    apply_score_effect_stage, EffectObservation, EmittedEffect, ScoreEffectStage, ScoreHit,
};
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::{MaskHit, MaskKind};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

pub struct TransitDeathMaskStage;

impl ScoreEffectStage for TransitDeathMaskStage {
    fn id(&self) -> StageId {
        StageId::TransitDeathMask
    }

    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect> {
        let actor_hp = ctx.scoring.active.hp();
        let pre_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        let mut emitted = Vec::new();
        for (plan_index, plan) in pool.plans.iter().enumerate() {
            if plan_has_lethal_transit(plan, actor_hp) {
                emitted.push(EmittedEffect {
                    plan_index,
                    hit: ScoreHit::Mask(MaskHit {
                        kind: MaskKind::Poison,
                        source: "transit_death",
                        original_score: None,
                    }),
                    observability: Some(EffectObservation::Contract(ContractMaskHit {
                        mask: "transit_death".into(),
                        original_score: pre_scores[plan_index],
                    })),
                });
            }
        }
        emitted
    }
}

impl PlanStage for TransitDeathMaskStage {
    fn name(&self) -> &'static str {
        "transit_death_mask"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_score_effect_stage(self, pool, ctx);
    }
}

pub fn apply_transit_death_mask(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    TransitDeathMaskStage.apply(pool, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::StageCtx;
    use crate::combat::ai::plan::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, snapshot_from, PoolBuilder,
        UnitBuilder,
    };
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::Entity;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::DiceRng;

    fn move_plan_with_self_damage(path: Vec<crate::game::hex::Hex>, self_damage: f32) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            outcomes: vec![StepOutcome {
                moved: true,
                self_damage,
                ..Default::default()
            }],
            final_pos: *path.last().unwrap_or(&hex_from_offset(0, 0)),
            ..Default::default()
        }
    }

    /// Run the stage in isolation on a solo-actor snapshot. Returns the pool
    /// after the stage has been applied.
    fn run_stage_simple(
        actor_hp: i32,
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
    ) -> crate::combat::ai::pipeline::ScoredPool {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(actor_hp)
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );
        let mut pool = PoolBuilder::new(plans)
            .scores(&scores)
            .trace_base_eq_score()
            .build();
        TransitDeathMaskStage.apply(&mut pool, &mut ctx);
        pool
    }

    #[test]
    fn lethal_transit_move_is_masked() {
        // Actor has 5 HP. Move step has 6 self-damage (lethal AoO in transit).
        // The wait plan (no outcomes) should remain selectable.
        let lethal_move = move_plan_with_self_damage(vec![hex_from_offset(-1, 0)], 6.0);
        let safe_wait = TurnPlan::default(); // no steps, no outcomes
        let pool = run_stage_simple(5, vec![lethal_move, safe_wait], vec![1.0, 0.5]);

        assert!(
            !pool.annotations[0].is_selectable(),
            "lethal transit move must be masked"
        );
        assert!(
            pool.annotations[1].is_selectable(),
            "safe wait plan must remain selectable"
        );
    }

    #[test]
    fn non_lethal_move_is_not_masked() {
        // Actor has 10 HP. Move deals only 3 self-damage (not lethal) → no mask.
        let safe_move = move_plan_with_self_damage(vec![hex_from_offset(-1, 0)], 3.0);
        let pool = run_stage_simple(10, vec![safe_move], vec![1.0]);

        assert!(
            pool.annotations[0].is_selectable(),
            "non-lethal move must not be masked"
        );
    }

    #[test]
    fn all_lethal_transit_graceful_degradation_no_panic() {
        // Every plan is a lethal transit — stage must not panic or empty the pool.
        let lethal1 = move_plan_with_self_damage(vec![hex_from_offset(-1, 0)], 5.0);
        let lethal2 = move_plan_with_self_damage(vec![hex_from_offset(1, 0)], 5.0);
        let pool = run_stage_simple(3, vec![lethal1, lethal2], vec![1.0, 0.8]);

        assert_eq!(pool.plans.len(), 2, "pool must not be emptied");
        assert!(
            !pool.annotations[0].is_selectable(),
            "plan[0] must be masked"
        );
        assert!(
            !pool.annotations[1].is_selectable(),
            "plan[1] must be masked"
        );
    }

    #[test]
    fn death_after_acting_cast_then_lethal_move_not_masked() {
        // Plan: [Cast (deals damage), Move (lethal AoO after cast)].
        // The Cast already executed → this is death-after-acting, NOT transit death.
        // Must NOT be masked.
        let target_entity = Entity::from_raw_u32(2).unwrap();
        let cast_then_lethal_move = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: target_entity,
                    target_pos: hex_from_offset(1, 0),
                },
                PlanStep::Move {
                    path: vec![hex_from_offset(-1, 0)],
                },
            ],
            outcomes: vec![
                // Cast step: actor deals damage, does not take self-damage
                StepOutcome {
                    moved: false,
                    self_damage: 0.0,
                    damage: 10.0,
                    ..Default::default()
                },
                // Move step: lethal AoO, but Cast already fired first
                StepOutcome {
                    moved: true,
                    self_damage: 6.0,
                    ..Default::default()
                },
            ],
            final_pos: hex_from_offset(-1, 0),
            ..Default::default()
        };
        let pool = run_stage_simple(5, vec![cast_then_lethal_move], vec![1.0]);

        assert!(
            pool.annotations[0].is_selectable(),
            "death-after-acting plan must NOT be masked (LastStand still eligible)"
        );
    }
}
