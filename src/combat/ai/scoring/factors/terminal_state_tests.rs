//! Tests for `terminal_state.rs` — split from the source file via `#[path]` in
//! `terminal_state.rs` (see end of that file). Production code stays in
//! `terminal_state.rs`; this file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../../docs/testing.md):
//! `terminal_state.rs` grew to 1054 LOC with tests dominating the lower half.
//!
//! `super::*` here resolves to `terminal_state.rs` (since this file is included
//! as `mod tests` inside terminal_state.rs).

use super::*;

use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::TerminalFactor;
use crate::combat::ai::test_helpers::{
    empty_maps, make_scoring_ctx, make_test_ctx, snapshot_from, UnitBuilder,
};
use crate::combat::ai::world::reservations::Reservations;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::world::tags::AiTags;
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
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps(); // danger map is all zeros
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let terminal = terminal_state_score(&plan, &snap, &ctx);
    assert_eq!(terminal.get(TerminalFactor::ExposureAtEnd), 0.0);
}

#[test]
fn exposure_at_end_high_in_dangerous_tile() {
    let actor_pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let mut maps = empty_maps();
    maps.danger.add(actor_pos, 0.8);
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let terminal = terminal_state_score(&plan, &snap, &ctx);
    assert!(
        (terminal.get(TerminalFactor::ExposureAtEnd) - 0.8).abs() < 1e-5,
        "expected ~0.8, got {}",
        terminal.get(TerminalFactor::ExposureAtEnd)
    );
}

/// `exposure_at_end` > 0 when the plan's final tile carries danger.
#[test]
fn exposure_at_end_non_zero_when_actor_in_enemy_threat_zone() {
    let actor_pos = hex_from_offset(0, 0);
    let enemy_adjacent = hex_from_offset(1, 0); // actor will end at actor_pos in danger
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let mut maps = empty_maps();
    // Simulate enemy threat: danger at actor's final position (enemy adjacent).
    maps.danger.add(actor_pos, 0.6);
    let _ = enemy_adjacent; // used conceptually above
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let exposure = compute_exposure_at_end(&plan, &ctx);
    assert!(
        exposure > 0.0,
        "exposure_at_end must be > 0 when final position is in enemy threat zone (danger=0.6), got {exposure}"
    );
}

/// `exposure_at_end` is ≈ 0 when actor stays in a safe backline tile
/// (danger map is zero at the final position).
#[test]
fn exposure_at_end_zero_in_safe_backline() {
    let actor_pos = hex_from_offset(5, 5); // far from any enemy
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps(); // danger map all zeros — safe backline
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let exposure = compute_exposure_at_end(&plan, &ctx);
    assert_eq!(
        exposure, 0.0,
        "exposure_at_end must be 0 in safe backline (danger map = 0)"
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
    let end_snap = snapshot_from(vec![dead_actor, enemy], 1);

    let initial_snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, end_snap);
    let terminal = terminal_state_score(&plan, &initial_snap, &ctx);
    assert_eq!(terminal.get(TerminalFactor::NextTurnLethality), 0.0);
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
    let snap = snapshot_from(vec![actor.clone(), far_enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let terminal = terminal_state_score(&plan, &snap, &ctx);
    assert_eq!(terminal.get(TerminalFactor::NextTurnLethality), 0.0);
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
    let snap = snapshot_from(vec![actor.clone(), enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let terminal = terminal_state_score(&plan, &snap, &ctx);
    assert!(
        terminal.get(TerminalFactor::NextTurnLethality) > 0.7,
        "expected high lethality, got {}",
        terminal.get(TerminalFactor::NextTurnLethality)
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
    let snap = snapshot_from(vec![actor.clone(), e1, e2], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let terminal = terminal_state_score(&plan, &snap, &ctx);
    assert_eq!(
        terminal.get(TerminalFactor::NextTurnLethality),
        1.0,
        "lethality must be clamped to 1.0, got {}",
        terminal.get(TerminalFactor::NextTurnLethality)
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
    let initial_snap = snapshot_from(vec![actor.clone(), far_enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor);

    let plan = deserialized_plan(actor_pos);
    // Empty sim_snapshots → uses initial_snap → enemy is far → lethality=0
    let terminal = terminal_state_score(&plan, &initial_snap, &ctx);
    assert_eq!(terminal.get(TerminalFactor::NextTurnLethality), 0.0);
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
    let plan = plan_with_outcomes(
        pos,
        vec![crate::combat::ai::outcome::ActionOutcomeEstimate {
            p_kill_now: 0.0,
            p_kill_soon: 0.0,
            ..Default::default()
        }],
    );
    assert_eq!(compute_secure_kill(&plan), 0.0);
}

#[test]
fn secure_kill_high_when_p_kill_now_one() {
    let pos = hex_from_offset(0, 0);
    let plan = plan_with_outcomes(
        pos,
        vec![crate::combat::ai::outcome::ActionOutcomeEstimate {
            p_kill_now: 1.0,
            p_kill_soon: 0.0,
            ..Default::default()
        }],
    );
    assert_eq!(compute_secure_kill(&plan), 1.0);
}

#[test]
fn secure_kill_partial_credit_for_kill_soon() {
    let pos = hex_from_offset(0, 0);
    let plan = plan_with_outcomes(
        pos,
        vec![crate::combat::ai::outcome::ActionOutcomeEstimate {
            p_kill_now: 0.0,
            p_kill_soon: 1.0,
            ..Default::default()
        }],
    );
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
    let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
        .full_hp(20)
        .build();
    let snap = snapshot_from(vec![actor.clone(), ally], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let ally_initial = UnitBuilder::new(2, Team::Enemy, ally_pos)
        .hp(4)
        .max_hp(20)
        .build();
    let ally_end = UnitBuilder::new(2, Team::Enemy, ally_pos)
        .hp(4)
        .max_hp(20)
        .build();
    let initial_snap = snapshot_from(vec![actor.clone(), ally_initial], 1);
    let end_snap = snapshot_from(vec![actor.clone(), ally_end], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let ally_initial = UnitBuilder::new(2, Team::Enemy, ally_pos)
        .hp(4)
        .max_hp(20)
        .build();
    let ally_end = UnitBuilder::new(2, Team::Enemy, ally_pos)
        .hp(16)
        .max_hp(20)
        .build();
    let initial_snap = snapshot_from(vec![actor.clone(), ally_initial], 1);
    let end_snap = snapshot_from(vec![actor.clone(), ally_end], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .hp(4)
        .max_hp(20)
        .build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
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
    assert!(
        score < 0.0,
        "expected negative gain when moving to worse tile, got {score}"
    );
}

// ── line_actionability ────────────────────────────────────────────────

#[test]
fn line_actionability_zero_when_no_abilities() {
    // Actor with empty abilities vec → max_range = 0 → score = 0.
    let actor_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build(); // abilities=[]
    let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
    let snap = snapshot_from(vec![actor.clone(), enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let score = compute_line_actionability(&plan, &snap, &ctx);
    assert_eq!(score, 0.0, "no abilities → no actionability");
}

#[test]
fn line_actionability_zero_when_actor_dead_at_end() {
    // Actor has abilities but is dead in end_snap → score = 0.
    let actor_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let actor_dead = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .hp(0)
        .ability_names(&["melee_attack"])
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
    let end_snap = snapshot_from(vec![actor_dead, enemy], 1);
    let actor_initial = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .ability_names(&["melee_attack"])
        .build();
    let initial_snap = snapshot_from(vec![actor_initial.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &initial_snap, &maps, &reservations, &actor_initial);

    let plan = idle_plan(actor_pos, end_snap);
    let score = compute_line_actionability(&plan, &initial_snap, &ctx);
    assert_eq!(score, 0.0, "dead actor → no actionability");
}

#[test]
fn line_actionability_zero_when_no_enemies_in_range() {
    // Actor has melee_attack (range=1); enemy is 5 tiles away → out of range.
    let actor_pos = hex_from_offset(0, 0);
    let far_enemy_pos = hex_from_offset(5, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .ability_names(&["melee_attack"]) // range=1
        .build();
    let far_enemy = UnitBuilder::new(2, Team::Player, far_enemy_pos).build();
    let snap = snapshot_from(vec![actor.clone(), far_enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let score = compute_line_actionability(&plan, &snap, &ctx);
    assert_eq!(score, 0.0, "enemy too far → no actionability");
}

#[test]
fn line_actionability_proportional_to_targets_in_range() {
    // Actor at (0,0) with fireball (range=5). Place 1 and then 3 enemies
    // within range to check proportional normalization.
    let actor_pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .ability_names(&["fireball"]) // range=5
        .build();
    let e1 = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
    let e2 = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0)).build();
    let e3 = UnitBuilder::new(4, Team::Player, hex_from_offset(3, 0)).build();
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();

    // 1 enemy in range → ~0.33
    let snap1 = snapshot_from(vec![actor.clone(), e1.clone()], 1);
    let ctx1 = make_scoring_ctx(&world, &snap1, &maps, &reservations, &actor);
    let plan1 = idle_plan(actor_pos, snap1.clone());
    let score1 = compute_line_actionability(&plan1, &snap1, &ctx1);
    assert!(
        (score1 - 1.0 / 3.0).abs() < 0.01,
        "1 enemy in range → expected ~0.33, got {score1}"
    );

    // 3 enemies in range → 1.0 (clamped)
    let snap3 = snapshot_from(vec![actor.clone(), e1, e2, e3], 1);
    let ctx3 = make_scoring_ctx(&world, &snap3, &maps, &reservations, &actor);
    let plan3 = idle_plan(actor_pos, snap3.clone());
    let score3 = compute_line_actionability(&plan3, &snap3, &ctx3);
    assert_eq!(
        score3, 1.0,
        "3 enemies in range → expected 1.0, got {score3}"
    );
}

// ── density_value ──────────────────────────────────────────────────────

#[test]
fn density_value_zero_for_non_aoe_actor() {
    // Actor without HAS_AOE tag → density_value = 0 regardless of enemies.
    let actor_pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build(); // tags=empty
    let e1 = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
    let e2 = UnitBuilder::new(3, Team::Player, hex_from_offset(0, 1)).build();
    let snap = snapshot_from(vec![actor.clone(), e1, e2], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let score = compute_density_value(&plan, &snap, &ctx);
    assert_eq!(score, 0.0, "non-AoE actor → density_value = 0");
}

#[test]
fn density_value_zero_when_no_cluster() {
    // AoE actor but all enemies are >2 tiles away → density_value = 0.
    let actor_pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .tags(AiTags::HAS_AOE)
        .build();
    let far_enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0)).build();
    let snap = snapshot_from(vec![actor.clone(), far_enemy], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let score = compute_density_value(&plan, &snap, &ctx);
    assert_eq!(score, 0.0, "no enemies in AoE radius → density_value = 0");
}

#[test]
fn density_value_high_when_3_enemies_in_radius() {
    // AoE actor; 3 enemies within radius=2 → density_value = 1.0.
    let actor_pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
        .tags(AiTags::HAS_AOE)
        .build();
    let e1 = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
    let e2 = UnitBuilder::new(3, Team::Player, hex_from_offset(0, 1)).build();
    let e3 = UnitBuilder::new(4, Team::Player, hex_from_offset(2, 0)).build();
    let snap = snapshot_from(vec![actor.clone(), e1, e2, e3], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps();
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(actor_pos, snap.clone());
    let score = compute_density_value(&plan, &snap, &ctx);
    assert_eq!(score, 1.0, "3 enemies in AoE radius → density_value = 1.0");
}

// ── pressure_spacing_zone ─────────────────────────────────────────────

#[test]
fn pressure_spacing_zero_when_pos_unchanged() {
    // final_pos == start_pos, ally_support is uniform → delta = 0.
    let pos = hex_from_offset(0, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let maps = empty_maps(); // ally_support all zeros
    let reservations = Reservations::default();
    let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

    let plan = idle_plan(pos, snap.clone()); // final_pos == actor start pos
    let score = compute_pressure_spacing_zone(&plan, &ctx);
    assert_eq!(score, 0.0, "no movement → spacing zone delta = 0");
}

#[test]
fn pressure_spacing_positive_when_moved_into_support() {
    // Actor moves from (0,0) to (1,0); ally_support is higher at (1,0).
    let start_pos = hex_from_offset(0, 0);
    let end_pos = hex_from_offset(1, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, start_pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let mut maps = empty_maps();
    maps.ally_support.add(end_pos, 0.6); // higher support at destination
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
    let score = compute_pressure_spacing_zone(&plan, &ctx);
    assert!(
        score > 0.0,
        "moved into ally support → positive pressure_spacing_zone, got {score}"
    );
}

#[test]
fn pressure_spacing_negative_when_moved_away() {
    // Actor moves from (0,0) to (1,0); ally_support is higher at start (0,0).
    let start_pos = hex_from_offset(0, 0);
    let end_pos = hex_from_offset(1, 0);
    let actor = UnitBuilder::new(1, Team::Enemy, start_pos).build();
    let snap = snapshot_from(vec![actor.clone()], 1);
    let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
    let difficulty = DifficultyProfile::hard();
    let world = make_test_ctx(&content, &difficulty);
    let mut maps = empty_maps();
    maps.ally_support.add(start_pos, 0.7); // higher support at origin
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
    let score = compute_pressure_spacing_zone(&plan, &ctx);
    assert!(
        score < 0.0,
        "moved away from ally support → negative pressure_spacing_zone, got {score}"
    );
}
