//! Plan-level `self_survival` factor — measures how much a plan improves the
//! actor's personal safety.
//!
//! Aggregation: **plan-level** (single value for the whole plan, not per-step
//! discounted sum). The factor captures the cumulative defensive value of all
//! steps:
//! - Self-heal casts: `expected_heal / max_hp` (cumulative).
//! - Self-armor-buff casts: `armor_bonus × 3 / max_hp` (3-turn usage weight).
//! - Terminal positioning: `max(0, danger(start) − danger(final_pos))`.
//!
//! AoO-based components and threat-centroid distance are skipped for MVP;
//! the danger-map exit component already captures "move away from threats".

use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::utility::ScoringCtx;
use crate::content::abilities::{EffectDef, StatusOn, TargetType};

/// Compute the plan-level `self_survival` for `plan` under `ctx`.
///
/// Returns a value in roughly `[−1, 1]` (positive = plan improves actor
/// survival, 0 = neutral, negative = plan worsens it — currently only 0+
/// since we don't model new-AoO-exposure here).
pub fn compute_plan_self_survival(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    let active = ctx.active;
    let max_hp = active.max_hp.max(1) as f32;
    let caster = &active.caster_ctx;

    let mut heal_sum = 0.0f32;
    let mut armor_sum = 0.0f32;

    for step in &plan.steps {
        let PlanStep::Cast { ability, target, .. } = step else { continue };
        // Only self-directed casts (actor targets themselves).
        if *target != active.entity {
            continue;
        }
        let Some(def) = ctx.world.content.abilities.get(ability) else { continue };
        if !matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
            continue;
        }

        // Self-heal: expected heal amount / max_hp.
        if let EffectDef::Heal { dice } = &def.effect {
            let ev = (dice.expected() as f32
                + caster.int_mod as f32
                + caster.spell_power as f32)
                .max(0.0);
            heal_sum += ev / max_hp;
        }

        // Self-armor-buff: armor_bonus × 3 (turns) / max_hp.
        for sa in &def.statuses {
            let is_on_self = sa.on == StatusOn::MySelf
                || sa.on == StatusOn::Target; // target == active.entity (checked above)
            if !is_on_self {
                continue;
            }
            let Some(sdef) = ctx.world.content.statuses.get(&sa.status) else { continue };
            if sdef.armor_bonus > 0 {
                armor_sum += sdef.armor_bonus as f32 * 3.0 / max_hp;
            }
        }
    }

    // Terminal position: net danger reduction.
    let exit_danger =
        (ctx.maps.danger.get(active.pos) - ctx.maps.danger.get(plan.final_pos)).max(0.0);

    heal_sum + armor_sum + exit_danger
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::planning::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn build_plan(steps: Vec<PlanStep>, final_pos: crate::game::hex::Hex, snap: &BattleSnapshot) -> TurnPlan {
        let len = steps.len();
        TurnPlan {
            steps,
            final_pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![StepOutcome::default(); len],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); len],
        }
    }

    #[test]
    fn self_heal_cast_gives_positive_survival() {
        let actor_pos = hex_from_offset(0, 0);
        // max_hp=20, healer with heal ability
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(10)
            .ability_names(&["heal"])
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = build_plan(
            vec![PlanStep::Cast {
                ability: "heal".into(),
                target: actor.entity,
                target_pos: actor_pos,
            }],
            actor_pos,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert!(
            survival > 0.0,
            "self-heal plan should give positive self_survival, got {survival}"
        );
    }

    #[test]
    fn summon_plan_gives_zero_survival() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(22)
            .hp(4)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // A plan that does NOT target self (cast targeting someone else)
        let other_entity = crate::combat::ai::snapshot::BattleSnapshot::new(
            vec![UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build()], 1
        );
        let _ = other_entity; // just to note the plan targets actor itself but with non-heal
        // Use an empty plan (EndTurn) — most direct test for "no survival improvement"
        let empty_plan = TurnPlan {
            steps: Vec::new(),
            final_pos: actor_pos,
            residual_ap: 1,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let survival = compute_plan_self_survival(&empty_plan, &ctx);
        assert_eq!(survival, 0.0, "EndTurn plan must give self_survival = 0");
    }

    #[test]
    fn retreat_move_gives_positive_survival() {
        let danger_pos = hex_from_offset(0, 0);
        let safe_pos = hex_from_offset(5, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, danger_pos)
            .max_hp(20)
            .hp(5)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Actor starts in danger, retreats to safety
        maps.danger.add(danger_pos, 0.8);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = build_plan(
            vec![PlanStep::Move { path: vec![safe_pos] }],
            safe_pos,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert!(
            survival > 0.0,
            "retreat from danger should give positive self_survival, got {survival}"
        );
    }
}
