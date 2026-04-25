//! SanityStage — step 7.1.
//!
//! Replicates `PlanRanking::apply_sanity` as a `PlanStage`. Applies
//! multiplicative sanity penalties/bonuses to `pool.scored` and writes the
//! per-plan hit breakdown into `pool.annotations[i].sanity`.

use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::sanity_adjust_plans;

pub struct SanityStage;

impl PlanStage for SanityStage {
    fn name(&self) -> &'static str {
        "sanity"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let breakdown = sanity_adjust_plans(&mut pool.scored, &pool.plans, ctx.scoring);
        for (ann, hits) in pool.annotations.iter_mut().zip(breakdown.into_iter()) {
            ann.sanity = hits;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use crate::core::DiceRng;

    fn make_move_plan(path: Vec<Hex>) -> TurnPlan {
        let final_pos = path.last().copied().unwrap_or_else(|| hex_from_offset(0, 0));
        TurnPlan {
            steps: vec![PlanStep::Move { path }],
            final_pos,
            ..TurnPlan::default()
        }
    }

    fn apply_sanity_to_two_plans(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        actor_hp: i32,
        actor_max_hp: i32,
        danger_on_final: Option<(Hex, f32)>,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(actor_hp)
            .max_hp(actor_max_hp)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        if let Some((tile, val)) = danger_on_final {
            maps.danger.add(tile, val);
        }
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
        pool.scored = scores;
        // raw_factors zeroed — sanity doesn't need them
        SanityStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── no hits on a clean plan ────────────────────────────────────────────

    #[test]
    fn sanity_stage_no_hits_leaves_annotation_empty() {
        // Two plans, full HP, no danger — no sanity rule fires.
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let pool = apply_sanity_to_two_plans(plans, vec![0.5, 0.4], 20, 20, None);

        for ann in &pool.annotations {
            assert!(
                ann.sanity.is_empty(),
                "expected no sanity hits for healthy actor in safe tile, got {:?}",
                ann.sanity,
            );
        }
    }

    // ── hits are written to annotation ────────────────────────────────────

    #[test]
    fn sanity_stage_appends_hits() {
        // Very low HP + high danger on destination triggers Survival rule.
        // We need >= 2 plans for sanity rules to run (early-return at len <= 1).
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        // 2/20 HP = 10%; danger tile on destination of plan 0
        let pool = apply_sanity_to_two_plans(
            plans, vec![0.5, 0.4], 2, 20, Some((dest_a, 1.0)),
        );

        // plan 0 ends on a dangerous tile with very low HP → Survival fires
        let hits_0 = &pool.annotations[0].sanity;
        assert!(
            !hits_0.is_empty(),
            "expected Survival hit for low-HP actor on danger tile, got empty",
        );
        assert!(
            hits_0.iter().any(|h| {
                use crate::combat::ai::planning::SanityRule;
                h.rule == SanityRule::Survival
            }),
            "expected Survival rule in hits, got {:?}", hits_0,
        );
    }

    fn empty_content() -> crate::content::content_view::ContentView {
        crate::combat::ai::test_helpers::empty_content()
    }
}
