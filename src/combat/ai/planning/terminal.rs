//! Terminal state evaluation — step 5 of ai-rework.
//!
//! One-shot per-plan evaluation of the final sim snapshot
//! (`plan.sim_snapshots.last()`). Independent of step-summed `PlanFactors`:
//! terminal axes capture "where we ended up", not "what we did along the way".
//!
//! Eight axes split into 3 clusters (5.1–5.3):
//!  - Defensive: `exposure_at_end`, `next_turn_lethality`
//!  - Offensive: `secure_kill`, `ally_rescue`, `board_control_gain`
//!  - Geometric: `line_actionability`, `density_value`, `pressure_spacing_zone`
//!
//! Step 5.0: scaffolding only — producer returns zeros, aggregator does not
//! read terminal scores yet. Wired into 5.4 via `axis_terminal_weights`.
//!
//! Step 5.1: defensive cluster — `exposure_at_end` + `next_turn_lethality`
//! implemented. Aggregator still inert (`axis_terminal_weights` = zeros).
//!
//! Step 5.2: offensive cluster — `secure_kill`, `ally_rescue`,
//! `board_control_gain` implemented. Aggregator still inert.
//!
//! Decomposition: docs/ai_rework_step5_plan.md.

use serde::{Deserialize, Serialize};

use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

/// Terminal-state evaluation per plan. Producer is `terminal_state_score`;
/// each axis populated incrementally in 5.1–5.3. Consumed in
/// `finalize_scores` (5.4) via `axis_terminal_weights` × `NeedSignals`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TerminalScore {
    pub exposure_at_end: f32,
    pub next_turn_lethality: f32,
    pub secure_kill: f32,
    pub ally_rescue: f32,
    pub board_control_gain: f32,
    pub line_actionability: f32,
    pub density_value: f32,
    pub pressure_spacing_zone: f32,
}

/// Compute the terminal-state score for a plan from its final sim snapshot.
///
/// Step 5.1: defensive cluster implemented — `exposure_at_end` and
/// `next_turn_lethality`.
/// Step 5.2: offensive cluster implemented — `secure_kill`, `ally_rescue`,
/// `board_control_gain`. Remaining 3 axes return 0.0 for step 5.3.
pub fn terminal_state_score(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> TerminalScore {
    let exposure_at_end = compute_exposure_at_end(plan, ctx);
    let next_turn_lethality = compute_next_turn_lethality(plan, initial_snap, ctx);
    let secure_kill = compute_secure_kill(plan);
    let ally_rescue = compute_ally_rescue(plan, initial_snap, ctx);
    let board_control_gain = compute_board_control_gain(plan, ctx);

    TerminalScore {
        exposure_at_end,
        next_turn_lethality,
        secure_kill,
        ally_rescue,
        board_control_gain,
        // TODO step 5.3: line_actionability, density_value, pressure_spacing_zone
        line_actionability: 0.0,
        density_value: 0.0,
        pressure_spacing_zone: 0.0,
    }
}

/// Danger map value at the actor's final position, clamped to [0, 1].
///
/// Even if the danger map is not normalised, clamp produces a safe [0, 1]
/// output. When the map is rank-normalised (see `InfluenceMap::normalize`),
/// the clamp is a no-op.
fn compute_exposure_at_end(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    ctx.maps.danger.get(plan.final_pos).clamp(0.0, 1.0)
}

// ── Step 5.2: offensive cluster ───────────────────────────────────────────────

/// Sum of kill confidence over all plan steps, clamped to [0, 1].
///
/// `p_kill_now` — confirmed kill from sim; `p_kill_soon` — DoT finishes it
/// next round. Half-weight on `p_kill_soon` reflects lower certainty.
/// Multiple kills can push the raw sum above 1.0, hence the `.min(1.0)`.
fn compute_secure_kill(plan: &TurnPlan) -> f32 {
    plan.annotation
        .outcomes
        .iter()
        .map(|o| o.p_kill_now + 0.5 * o.p_kill_soon)
        .sum::<f32>()
        .min(1.0)
}

/// Credit for having rescued an endangered ally during this turn.
///
/// An ally is "endangered" if at plan start they were below 40% HP *and* their
/// tile had danger > 0.5 (i.e. genuinely threatened, not just low HP by
/// attrition). If by plan end the same ally is above 60% HP, we credit
/// `1 − initial_hp_pct` — proportional to how dire the situation was.
///
/// The actor itself is excluded (self-preservation is captured in
/// `exposure_at_end` / `next_turn_lethality`). Clamped to [0, 1] in case
/// multiple rescues accumulate.
///
/// Thresholds (0.4, 0.5, 0.6) are hard-coded pending 5.4–5.5 `Thresholds`
/// struct.
fn compute_ally_rescue(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> f32 {
    let end_snap = plan.sim_snapshots.last().unwrap_or(initial_snap);
    let mut total = 0.0_f32;

    for ally_initial in initial_snap.allies_of(ctx.active.team) {
        // Skip self — ally_rescue is about *other* friendlies.
        if ally_initial.entity == ctx.active.entity {
            continue;
        }
        let was_endangered = ally_initial.hp_pct() < 0.4
            && ctx.maps.danger.get(ally_initial.pos) > 0.5;
        if !was_endangered {
            continue;
        }
        if let Some(ally_end) = end_snap.unit(ally_initial.entity) {
            if ally_end.hp_pct() > 0.6 {
                // Credit proportional to how endangered they were.
                total += (1.0 - ally_initial.hp_pct()).max(0.0);
            }
        }
    }

    total.min(1.0)
}

/// Signed change in opportunity-map value between start and final position.
///
/// Positive → moved to a strategically better tile; negative → retreated to a
/// worse one. Clamped to [−1, 1] so that extreme swings stay comparable with
/// the other [0, 1] axes once the aggregator is activated in 5.4.
///
/// The penalty for moving to a worse tile is intentional: `board_control_gain`
/// should discourage purely retreating Repostion plans if the axis weight is
/// positive. The aggregator context determines the final effect.
fn compute_board_control_gain(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    let start_op = ctx.maps.opportunity.get(ctx.active.pos);
    let end_op = ctx.maps.opportunity.get(plan.final_pos);
    (end_op - start_op).clamp(-1.0, 1.0)
}

/// Fraction of actor's remaining HP that can be dealt by reachable enemies
/// next turn, clamped to [0, 1].
///
/// "Reachable" = enemy speed + max_attack_range covers `plan.final_pos`.
/// DPR estimate uses `horizon_avg` — the same metric used in intent scoring
/// and trade evaluation, so weights are consistent.
///
/// Returns 0.0 if the actor is dead by end of plan (no point estimating
/// incoming threat for a corpse).
fn compute_next_turn_lethality(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> f32 {
    let end_snap = plan.sim_snapshots.last().unwrap_or(initial_snap);
    let actor_id = ctx.active.entity;

    // If the actor died during the plan, threat at end_pos is irrelevant.
    let actor_hp_at_end = match end_snap.unit(actor_id) {
        Some(u) if u.hp > 0 => u.hp,
        _ => return 0.0,
    };

    let final_pos = plan.final_pos;
    let dpr_sum: f32 = end_snap
        .enemies_of(ctx.active.team)
        .filter(|e| e.hp > 0)
        .filter(|e| {
            let reach = (e.speed.max(0) as u32).saturating_add(e.max_attack_range);
            final_pos.unsigned_distance_to(e.pos) <= reach
        })
        .map(crate::combat::ai::scoring::horizon_avg)
        .sum();

    // lethality > 1.0 means "likely dead next turn"; clamp to [0, 1].
    (dpr_sum / actor_hp_at_end as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Minimal plan: no steps, actor stays at `pos`.
    fn idle_plan(pos: crate::game::hex::Hex, snap: BattleSnapshot) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            final_pos: pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![snap],
            annotation: Default::default(),
        }
    }

    /// Plan with no sim_snapshots (deserialized shape) ending at `pos`.
    fn deserialized_plan(pos: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            final_pos: pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // ── exposure_at_end ────────────────────────────────────────────────────

    #[test]
    fn exposure_at_end_zero_when_no_danger() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps(); // danger map is all zeros
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let terminal = terminal_state_score(&plan, &snap, &ctx);
        assert_eq!(terminal.exposure_at_end, 0.0);
    }

    #[test]
    fn exposure_at_end_high_in_dangerous_tile() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        maps.danger.add(actor_pos, 0.8);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let terminal = terminal_state_score(&plan, &snap, &ctx);
        assert!(
            (terminal.exposure_at_end - 0.8).abs() < 1e-5,
            "expected ~0.8, got {}",
            terminal.exposure_at_end
        );
    }

    // ── next_turn_lethality ────────────────────────────────────────────────

    #[test]
    fn next_turn_lethality_zero_when_actor_dead_at_end() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        // End snapshot: actor has hp=0 (dead by plan's end)
        let dead_actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(0).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .speed(3)
            .threat(10.0)
            .build();
        let end_snap = BattleSnapshot::new(vec![dead_actor, enemy], 1);

        let initial_snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, end_snap);
        let terminal = terminal_state_score(&plan, &initial_snap, &ctx);
        assert_eq!(terminal.next_turn_lethality, 0.0);
    }

    #[test]
    fn next_turn_lethality_zero_when_no_enemies_in_reach() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(20).build();
        // Enemy far away: speed=2, max_attack_range=1 → reach=3; distance=10 > 3
        let far_enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(10, 0))
            .speed(2)
            .max_attack_range(1)
            .threat(8.0)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), far_enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let terminal = terminal_state_score(&plan, &snap, &ctx);
        assert_eq!(terminal.next_turn_lethality, 0.0);
    }

    #[test]
    fn next_turn_lethality_high_when_dpr_exceeds_hp() {
        let actor_pos = hex_from_offset(0, 0);
        // Actor with low HP
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(5).build();
        // Adjacent enemy with high threat (horizon_avg falls back to threat)
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .speed(3)
            .max_attack_range(1)
            .threat(12.0) // DPR=12, actor HP=5 → ratio=2.4 → clamped to 1.0
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let terminal = terminal_state_score(&plan, &snap, &ctx);
        assert!(
            terminal.next_turn_lethality > 0.7,
            "expected high lethality, got {}",
            terminal.next_turn_lethality
        );
    }

    #[test]
    fn next_turn_lethality_clamped_to_one() {
        let actor_pos = hex_from_offset(0, 0);
        // Actor with 1 HP, many strong adjacent enemies
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(1).build();
        let e1 = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .speed(5)
            .max_attack_range(2)
            .threat(20.0)
            .build();
        let e2 = UnitBuilder::new(3, Team::Player, hex_from_offset(0, 1))
            .speed(5)
            .max_attack_range(2)
            .threat(20.0)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), e1, e2], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let terminal = terminal_state_score(&plan, &snap, &ctx);
        assert_eq!(
            terminal.next_turn_lethality,
            1.0,
            "lethality must be clamped to 1.0, got {}",
            terminal.next_turn_lethality
        );
    }

    #[test]
    fn next_turn_lethality_uses_initial_snap_when_sim_snapshots_empty() {
        // Deserialized plan: sim_snapshots is empty → fallback to initial_snap.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(20).build();
        let far_enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(10, 0))
            .speed(2)
            .max_attack_range(1)
            .threat(8.0)
            .build();
        let initial_snap = BattleSnapshot::new(vec![actor.clone(), far_enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

        let plan = deserialized_plan(actor_pos);
        // Empty sim_snapshots → uses initial_snap → enemy is far → lethality=0
        let terminal = terminal_state_score(&plan, &initial_snap, &ctx);
        assert_eq!(terminal.next_turn_lethality, 0.0);
    }

    // ── secure_kill ────────────────────────────────────────────────────────

    /// Helper: plan with given annotation outcomes, actor at `pos`.
    fn plan_with_outcomes(
        pos: crate::game::hex::Hex,
        outcomes: Vec<crate::combat::ai::outcome::ActionOutcomeEstimate>,
    ) -> TurnPlan {
        TurnPlan {
            steps: vec![],
            final_pos: pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![],
            annotation: crate::combat::ai::outcome::PlanAnnotation {
                outcomes,
                ..Default::default()
            },
        }
    }

    #[test]
    fn secure_kill_zero_for_no_kill_plan() {
        let pos = hex_from_offset(0, 0);
        let plan = plan_with_outcomes(pos, vec![
            crate::combat::ai::outcome::ActionOutcomeEstimate {
                p_kill_now: 0.0,
                p_kill_soon: 0.0,
                ..Default::default()
            },
        ]);
        assert_eq!(compute_secure_kill(&plan), 0.0);
    }

    #[test]
    fn secure_kill_high_when_p_kill_now_one() {
        let pos = hex_from_offset(0, 0);
        let plan = plan_with_outcomes(pos, vec![
            crate::combat::ai::outcome::ActionOutcomeEstimate {
                p_kill_now: 1.0,
                p_kill_soon: 0.0,
                ..Default::default()
            },
        ]);
        assert_eq!(compute_secure_kill(&plan), 1.0);
    }

    #[test]
    fn secure_kill_partial_credit_for_kill_soon() {
        let pos = hex_from_offset(0, 0);
        let plan = plan_with_outcomes(pos, vec![
            crate::combat::ai::outcome::ActionOutcomeEstimate {
                p_kill_now: 0.0,
                p_kill_soon: 1.0,
                ..Default::default()
            },
        ]);
        let score = compute_secure_kill(&plan);
        assert!(
            (score - 0.5).abs() < 1e-5,
            "expected 0.5 for p_kill_soon=1.0, got {score}"
        );
    }

    #[test]
    fn secure_kill_clamped_to_one_for_multiple_kills() {
        let pos = hex_from_offset(0, 0);
        // Three outcomes each with p_kill_now=0.7 → raw sum = 2.1 → clamped to 1.0
        let outcome = crate::combat::ai::outcome::ActionOutcomeEstimate {
            p_kill_now: 0.7,
            p_kill_soon: 0.0,
            ..Default::default()
        };
        let plan = plan_with_outcomes(pos, vec![outcome.clone(), outcome.clone(), outcome]);
        assert_eq!(compute_secure_kill(&plan), 1.0);
    }

    // ── ally_rescue ────────────────────────────────────────────────────────

    #[test]
    fn ally_rescue_zero_when_no_endangered_ally() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        // Ally has full HP — not endangered
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0)).full_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), ally], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let score = compute_ally_rescue(&plan, &snap, &ctx);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn ally_rescue_zero_when_endangered_ally_still_low_at_end() {
        let actor_pos = hex_from_offset(0, 0);
        let ally_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        // Ally at 20% HP — endangered; stays low in end snap
        let ally_initial = UnitBuilder::new(2, Team::Enemy, ally_pos).hp(4).max_hp(20).build();
        let ally_end = UnitBuilder::new(2, Team::Enemy, ally_pos).hp(4).max_hp(20).build();
        let initial_snap = BattleSnapshot::new(vec![actor.clone(), ally_initial], 1);
        let end_snap = BattleSnapshot::new(vec![actor.clone(), ally_end], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // High danger at ally position
        maps.danger.add(ally_pos, 0.8);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, end_snap);
        let score = compute_ally_rescue(&plan, &initial_snap, &ctx);
        assert_eq!(score, 0.0, "no rescue — ally still at low HP at end");
    }

    #[test]
    fn ally_rescue_credits_low_hp_to_safe_transition() {
        let actor_pos = hex_from_offset(0, 0);
        let ally_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        // Ally at 20% HP initially (4/20), recovered to 80% (16/20) at end
        let ally_initial = UnitBuilder::new(2, Team::Enemy, ally_pos).hp(4).max_hp(20).build();
        let ally_end = UnitBuilder::new(2, Team::Enemy, ally_pos).hp(16).max_hp(20).build();
        let initial_snap = BattleSnapshot::new(vec![actor.clone(), ally_initial], 1);
        let end_snap = BattleSnapshot::new(vec![actor.clone(), ally_end], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        maps.danger.add(ally_pos, 0.8);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, end_snap);
        let score = compute_ally_rescue(&plan, &initial_snap, &ctx);
        // credit = 1.0 - 0.2 = 0.8
        assert!(
            (score - 0.8).abs() < 1e-5,
            "expected ~0.8 for rescue from 20% hp, got {score}"
        );
    }

    #[test]
    fn ally_rescue_skips_self() {
        let actor_pos = hex_from_offset(0, 0);
        // Actor itself is at low HP and in high danger — should be ignored
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(4).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        maps.danger.add(actor_pos, 0.9);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let score = compute_ally_rescue(&plan, &snap, &ctx);
        assert_eq!(score, 0.0, "actor's own rescue should not count");
    }

    // ── board_control_gain ────────────────────────────────────────────────

    #[test]
    fn board_control_gain_zero_when_pos_unchanged() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps(); // opportunity all zeros
        let reservations = Reservations::default();
        let snap_for_ctx = snap.clone();
        let ctx = make_scoring_ctx(&world, &snap_for_ctx, &maps, &reservations, &actor);

        let plan = idle_plan(pos, snap); // final_pos == actor.pos
        let score = compute_board_control_gain(&plan, &ctx);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn board_control_gain_positive_when_moved_to_better() {
        let start_pos = hex_from_offset(0, 0);
        let end_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, start_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        maps.opportunity.add(end_pos, 0.7); // end tile is better
        let reservations = Reservations::default();
        let snap_for_ctx = snap.clone();
        let ctx = make_scoring_ctx(&world, &snap_for_ctx, &maps, &reservations, &actor);

        // Plan ends at end_pos
        let plan = TurnPlan {
            steps: vec![],
            final_pos: end_pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![snap],
            annotation: Default::default(),
        };
        let score = compute_board_control_gain(&plan, &ctx);
        assert!(score > 0.0, "expected positive gain, got {score}");
    }

    #[test]
    fn board_control_gain_negative_when_moved_to_worse() {
        let start_pos = hex_from_offset(0, 0);
        let end_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, start_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        maps.opportunity.add(start_pos, 0.8); // start tile is better
        let reservations = Reservations::default();
        let snap_for_ctx = snap.clone();
        let ctx = make_scoring_ctx(&world, &snap_for_ctx, &maps, &reservations, &actor);

        let plan = TurnPlan {
            steps: vec![],
            final_pos: end_pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: vec![snap],
            annotation: Default::default(),
        };
        let score = compute_board_control_gain(&plan, &ctx);
        assert!(score < 0.0, "expected negative gain when moving to worse tile, got {score}");
    }
}
