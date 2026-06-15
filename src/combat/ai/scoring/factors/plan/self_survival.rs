//! `PlanFactor::SelfSurvival` — how much the plan improves the actor's survival.
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
//!
//! `_intent` is unused — required for `factor_kind!` macro uniformity.

pub const NAME: &str = "self_survival";
pub const SIGNED: bool = false;

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::plan::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::content::abilities::{EffectDef, StatusOn, TargetType};
use crate::game::hex::Hex;

pub fn compute(plan: &TurnPlan, _intent: &TacticalIntent, ctx: &ScoringCtx) -> f32 {
    compute_plan_self_survival(plan, ctx)
}

/// Compute the plan-level `self_survival` for `plan` under `ctx`.
///
/// Returns a value in roughly `[−1, 1]` (positive = plan improves actor
/// survival, 0 = neutral, negative = plan worsens it — currently only 0+
/// since we don't model new-AoO-exposure here).
///
/// # Overlap note (5.5)
/// `compute_plan_self_survival` ≠ `terminal::next_turn_lethality`: this
/// factor measures *what the plan does to improve the actor's defences* —
/// self-heals, armor-buffs, and moving out of danger (danger[start] −
/// danger[committed_final_pos]). `next_turn_lethality` measures *how much
/// incoming threat awaits the actor at their final position next turn* —
/// Σ enemy DPR reachable at end_pos / actor_hp_at_end. The two signals are
/// complementary: `self_survival` is proactive (did we improve?), lethality
/// is reactive (how bad is the endpoint?). Keep both.
pub fn compute_plan_self_survival(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    let active = ctx.active;
    let max_hp = active.max_hp().max(1) as f32;
    let caster = &active.cache.caster_ctx;

    let mut heal_sum = 0.0f32;
    let mut armor_sum = 0.0f32;

    // Only count steps in the committed prefix — phantom-tail steps (Move after
    // a committed Cast, or extra steps after a MoveAndCast bundle) are never
    // executed by `commit_plan` and must not inflate self_survival.
    let prefix_len = plan.committed_step_count();

    for (idx, step) in plan.steps.iter().enumerate() {
        if idx >= prefix_len {
            break; // phantom tail — stop aggregating
        }
        let PlanStep::Cast {
            ability, target, ..
        } = step
        else {
            continue;
        };
        // Only self-directed casts (actor targets themselves).
        if *target != active.entity() {
            continue;
        }
        let Some(def) = ctx.world.content.abilities.get(ability) else {
            continue;
        };
        if !matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
            continue;
        }

        // Self-heal: expected heal amount / max_hp.
        if let EffectDef::Heal { dice } = &def.effect {
            let ev = (dice.expected() + caster.int_mod as f32 + caster.spell_power as f32).max(0.0);
            heal_sum += ev / max_hp;
        }

        // Self-armor-buff: armor_bonus × 3 (turns) / max_hp.
        for sa in &def.statuses {
            let is_on_self = sa.on == StatusOn::MySelf || sa.on == StatusOn::Target; // target == active.entity (checked above)
            if !is_on_self {
                continue;
            }
            let Some(sdef) = ctx.world.content.statuses.get(&sa.status) else {
                continue;
            };
            if sdef.bonuses.runtime.0.armor > 0 {
                armor_sum += sdef.bonuses.runtime.0.armor as f32 * 3.0 / max_hp;
            }
        }
    }

    // Terminal position: net danger reduction from start to committed-prefix end.
    // Using committed_prefix_final_pos instead of plan.final_pos prevents phantom
    // retreat steps (Move after a committed Cast) from inflating exit_danger.
    let committed_final = committed_prefix_final_pos(plan, active.pos);
    let exit_danger =
        (ctx.maps.danger.get(active.pos) - ctx.maps.danger.get(committed_final)).max(0.0);

    heal_sum + armor_sum + exit_danger
}

/// Return the actor's position after the committed prefix fires.
///
/// Mirrors the rules of `commit_plan` in `planning/picker.rs`:
/// - `[]`                → `actor_pos` (EndTurn, actor didn't move)
/// - `[Cast, ...]`       → `actor_pos` (CastInPlace, caster unchanged)
/// - `[Move, Cast, ...]` → last tile of first Move path (MoveAndCast bundle)
/// - `[Move, ...]`       → last tile of first Move path (MoveOnly)
fn committed_prefix_final_pos(plan: &TurnPlan, actor_pos: Hex) -> Hex {
    match plan.committed_prefix() {
        CommittedPrefix::EndTurn | CommittedPrefix::Cast { .. } => actor_pos,
        CommittedPrefix::MoveThenCast { path, .. } | CommittedPrefix::MoveOnly { path } => {
            path.last().copied().unwrap_or(actor_pos)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::plan::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn build_plan(
        steps: Vec<PlanStep>,
        final_pos: crate::game::hex::Hex,
        snap: &BattleSnapshot,
    ) -> TurnPlan {
        let len = steps.len();
        TurnPlan {
            steps,
            final_pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![StepOutcome::default(); len],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); len],
            annotation: Default::default(),
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
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
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
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // A plan that does NOT target self (cast targeting someone else)
        let other_entity = snapshot_from(
            vec![UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build()],
            1,
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
            annotation: Default::default(),
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
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Actor starts in danger, retreats to safety
        maps.danger.add(danger_pos, 0.8);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = build_plan(
            vec![PlanStep::Move {
                path: vec![safe_pos],
            }],
            safe_pos,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert!(
            survival > 0.0,
            "retreat from danger should give positive self_survival, got {survival}"
        );
    }

    // ── Phantom-tail regression tests ────────────────────────────────────────

    /// Regression pin: solo committed Cast targeting self still gives credit.
    #[test]
    fn self_heal_in_committed_cast_counts() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(10)
            .ability_names(&["heal"])
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Cast heal(self)] — committed prefix = Cast, credit counts
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
            "committed self-heal cast must give positive self_survival, got {survival}"
        );
    }

    /// Phantom tail: second Cast (self-heal) after a committed Cast is not executed
    /// and must not inflate self_survival.
    #[test]
    fn self_heal_in_phantom_tail_does_not_count() {
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(10)
            .ability_names(&["melee_attack", "heal"])
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps(); // no danger anywhere
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Cast melee_attack @ enemy, Cast heal @ self]
        // Committed prefix = step 0 (solo Cast @ enemy). Step 1 is phantom tail.
        // Step 0 targets enemy → no self-credit. Step 1 phantom → no credit.
        // exit_danger: actor didn't move → 0.
        let plan = build_plan(
            vec![
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: enemy.entity,
                    target_pos: enemy_pos,
                },
                PlanStep::Cast {
                    ability: "heal".into(),
                    target: actor.entity,
                    target_pos: actor_pos,
                },
            ],
            actor_pos,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert_eq!(
            survival, 0.0,
            "phantom-tail self-heal must not contribute; got {survival}"
        );
    }

    /// Key regression guard for the corpus leaks: [Cast @ enemy, Move retreat].
    /// Committed prefix = Cast only (actor didn't move). exit_danger must be 0
    /// even though the phantom retreat destination has low danger.
    #[test]
    fn exit_danger_uses_committed_prefix_end() {
        let actor_pos = hex_from_offset(0, 0);
        let retreat_pos = hex_from_offset(5, 0);
        let enemy_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(1)
            .ability_names(&["melee_attack"])
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Actor starts in high danger; retreat destination has low danger.
        // Under the old code, exit_danger would credit the phantom retreat.
        maps.danger.add(actor_pos, 0.88);
        maps.danger.add(retreat_pos, 0.30);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Cast melee_attack @ enemy, Move retreat]
        // Committed prefix = step 0 (Cast). Caster position unchanged after commit.
        // exit_danger = danger(actor_pos) - danger(actor_pos) = 0.
        let plan = build_plan(
            vec![
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: enemy.entity,
                    target_pos: enemy_pos,
                },
                PlanStep::Move {
                    path: vec![retreat_pos],
                },
            ],
            retreat_pos, // plan.final_pos (the old code would use this)
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert_eq!(
            survival, 0.0,
            "committed prefix is Cast-only — phantom retreat must not give exit_danger credit; got {survival}"
        );
    }

    /// MoveAndCast bundle: both Move and Cast commit. exit_danger uses the
    /// move destination, not the actor's start tile.
    #[test]
    fn move_and_cast_bundle_counts_move_destination() {
        let actor_pos = hex_from_offset(0, 0);
        let tile_b = hex_from_offset(3, 0);
        let enemy_pos = hex_from_offset(4, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(5)
            .ability_names(&["melee_attack"])
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Actor starts safe, tile_b is also safe — no danger anywhere relevant.
        // The point of the test is that committed_prefix_final_pos = tile_b.
        // Give actor_pos some danger so we can detect if start-tile is wrongly used.
        maps.danger.add(actor_pos, 0.5);
        // tile_b has danger 0.0 (empty) → exit_danger = 0.5 − 0.0 = 0.5
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Move→tile_b, Cast enemy] — MoveAndCast bundle, prefix_len=2
        let plan = build_plan(
            vec![
                PlanStep::Move { path: vec![tile_b] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: enemy.entity,
                    target_pos: enemy_pos,
                },
            ],
            tile_b,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        // exit_danger uses tile_b (danger=0.0), not actor_pos (danger=0.5)
        // heal_sum=0, armor_sum=0 → survival = 0.5
        assert!(
            (survival - 0.5).abs() < 1e-5,
            "MoveAndCast: exit_danger should use tile_b destination; got {survival}"
        );
    }

    /// MoveOnly: only the first Move commits; second Move is phantom tail.
    /// committed_prefix_final_pos = tile_a (first move end), not tile_b.
    #[test]
    fn move_only_counts_first_move_destination() {
        let actor_pos = hex_from_offset(0, 0);
        let tile_a = hex_from_offset(2, 0);
        let tile_b = hex_from_offset(5, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(5)
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Actor starts in danger, tile_a is intermediate (still some danger),
        // tile_b is the phantom safe destination.
        maps.danger.add(actor_pos, 0.8);
        maps.danger.add(tile_a, 0.6);
        // tile_b has 0 danger (safe) — must NOT be used for exit_danger
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Move→tile_a, Move→tile_b] — MoveOnly commits first Move; second is phantom
        let plan = build_plan(
            vec![
                PlanStep::Move { path: vec![tile_a] },
                PlanStep::Move { path: vec![tile_b] },
            ],
            tile_b, // plan.final_pos would give exit_danger=0.8 (wrong)
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        // Committed final pos = tile_a (danger=0.6) → exit_danger = 0.8 − 0.6 = 0.2
        assert!(
            (survival - 0.2).abs() < 1e-5,
            "MoveOnly: exit_danger must use first-Move destination (tile_a), got {survival}"
        );
    }

    /// Phantom Cast after MoveAndCast bundle must not contribute armor_sum.
    #[test]
    fn armor_buff_in_phantom_cast_ignored() {
        let actor_pos = hex_from_offset(0, 0);
        let tile_b = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_hp(20)
            .hp(10)
            .ability_names(&["melee_attack", "taunt"])
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps(); // no danger
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // [Move→tile_b, Cast melee_attack @ enemy, Cast taunt @ self]
        // Committed prefix = MoveAndCast (steps 0+1, prefix_len=2).
        // Step 2 (taunt self → armor_buff) is phantom tail → armor_sum must stay 0.
        // exit_danger = 0 (no danger anywhere), heal_sum = 0 → survival = 0.
        let plan = build_plan(
            vec![
                PlanStep::Move { path: vec![tile_b] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: enemy.entity,
                    target_pos: enemy_pos,
                },
                PlanStep::Cast {
                    ability: "taunt".into(),
                    target: actor.entity,
                    target_pos: tile_b,
                },
            ],
            tile_b,
            &snap,
        );
        let survival = compute_plan_self_survival(&plan, &ctx);
        assert_eq!(
            survival, 0.0,
            "phantom-tail armor buff must not contribute; got {survival}"
        );
    }

    /// `self_survival_ignores_intent_parameter` — verifies the `_intent` param doesn't affect output.
    #[test]
    fn self_survival_ignores_intent_parameter() {
        use crate::combat::ai::intent::TacticalIntent;
        use crate::combat::ai::outcome::PlanAnnotation;
        use bevy::prelude::Entity;

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let diff = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let active = UnitBuilder::new(0, Team::Enemy, tile).build();
        let snap = snapshot_from(vec![active.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &active);

        let plan = TurnPlan {
            steps: vec![],
            annotation: PlanAnnotation::default(),
            outcomes: vec![],
            sim_snapshots: vec![],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            partial_score: 0.0,
        };

        // Two different intents should produce identical self_survival.
        let dummy_target = Entity::from_raw_u32(99).unwrap();
        let intent_a = TacticalIntent::Reposition;
        let intent_b = TacticalIntent::FocusTarget {
            target: dummy_target,
        };

        let score_a = compute(&plan, &intent_a, &ctx);
        let score_b = compute(&plan, &intent_b, &ctx);
        assert_eq!(
            score_a, score_b,
            "self_survival must not depend on intent; got {score_a} vs {score_b}"
        );
    }
}
