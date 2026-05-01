//! Terminal state evaluation — one-shot per-plan assessment of the final sim snapshot.
//!
//! Independent of step-summed factors: terminal axes capture "where we ended up",
//! not "what we did along the way". Eight axes, three clusters:
//! - Defensive: `exposure_at_end`, `next_turn_lethality`.
//! - Offensive: `secure_kill`, `ally_rescue`, `board_control_gain`.
//! - Geometric: `line_actionability`, `density_value`, `pressure_spacing_zone`.
//!
//! As of schema v29 (step 8.A), `terminal_state_score` returns a registry-typed
//! `FactorTerminalScore` (`factors::TerminalScore`). The legacy `TerminalScore`
//! named struct has been removed; use `FactorTerminalScore` everywhere.
//!
//! The per-axis `compute_*` free functions remain here as `pub(crate)` helpers;
//! they are used by the `factors::terminal` leaf modules.

use crate::combat::ai::scoring::factors::{FactorTerminalScore, TerminalFactor};
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::world::snapshot::{AiTags, BattleSnapshot};
use crate::combat::ai::utility::ScoringCtx;

/// Compute the terminal-state score for a plan from its final sim snapshot.
///
/// Returns a `FactorTerminalScore` (registry-typed wrapper) as of schema v29.
/// All 8 axes populated as of step 5.3.
pub fn terminal_state_score(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> FactorTerminalScore {
    let mut out = FactorTerminalScore::default();
    out.set(TerminalFactor::ExposureAtEnd,     compute_exposure_at_end(plan, ctx));
    out.set(TerminalFactor::NextTurnLethality, compute_next_turn_lethality(plan, initial_snap, ctx));
    out.set(TerminalFactor::SecureKill,        compute_secure_kill(plan));
    out.set(TerminalFactor::AllyRescue,        compute_ally_rescue(plan, initial_snap, ctx));
    out.set(TerminalFactor::BoardControlGain,  compute_board_control_gain(plan, ctx));
    out.set(TerminalFactor::LineActionability, compute_line_actionability(plan, initial_snap, ctx));
    out.set(TerminalFactor::DensityValue,      compute_density_value(plan, initial_snap, ctx));
    out.set(TerminalFactor::PressureSpacingZone, compute_pressure_spacing_zone(plan, ctx));
    out
}

/// Danger map value at the actor's final position, clamped to [0, 1].
///
/// Even if the danger map is not normalised, clamp produces a safe [0, 1]
/// output. When the map is rank-normalised (see `InfluenceMap::normalize`),
/// the clamp is a no-op.
pub(crate) fn compute_exposure_at_end(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    ctx.maps.danger.get(plan.final_pos).clamp(0.0, 1.0)
}

// ── Step 5.2: offensive cluster ───────────────────────────────────────────────

/// Sum of kill confidence over all plan steps, clamped to [0, 1].
///
/// `p_kill_now` — confirmed kill from sim; `p_kill_soon` — DoT finishes it
/// next round. Half-weight on `p_kill_soon` reflects lower certainty.
/// Multiple kills can push the raw sum above 1.0, hence the `.min(1.0)`.
///
/// # Overlap note (5.5)
/// The `factors::offensive` step factors also read `p_kill_now`/`p_kill_soon`
/// and contribute `kill_now`/`kill_promised` to the per-step discounted sum
/// in `PlanFactorValues`. This creates a logical overlap: both pathways credit the
/// same kills. The distinction is *aggregation*: step factors apply a depth
/// discount (`base^k`), so kills on steps 2-3 are underweighted relative to
/// kills on step 1. `secure_kill` is a flat roll-up over the whole plan —
/// it treats every kill equally regardless of step depth, making it sensitive
/// to multi-step kill combos that the discounted step sum undervalues.
/// Keep both — they measure related but different things. Double-counting risk
/// is mitigated by the separate weight tables (`axis_factor_weights` vs
/// `axis_terminal_weights`) which are tuned independently.
pub(crate) fn compute_secure_kill(plan: &TurnPlan) -> f32 {
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
pub(crate) fn compute_ally_rescue(
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
pub(crate) fn compute_board_control_gain(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
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
pub(crate) fn compute_next_turn_lethality(
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

// ── Step 5.3: geometric cluster ───────────────────────────────────────────────

/// How many enemies are within max cast range from the actor's end position,
/// normalised to [0, 1] (≥3 enemies → 1.0).
///
/// Uses the max range across all offensive and ground-targeted abilities (the
/// same set used by `max_attack_range` in snapshot building). Returns 0.0 if
/// the actor is dead at end of plan or has no abilities with range > 0.
///
/// "Actionability" measures how well the actor is positioned to act on the
/// next turn without having to move first — a proxy for staying in the fight.
///
/// TODO(5.5): if abilities change during plan (e.g. summon expiry), re-derive
/// from end_snap. For the current content set, abilities are static per turn.
pub(crate) fn compute_line_actionability(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> f32 {
    let end_snap = plan.sim_snapshots.last().unwrap_or(initial_snap);

    // Bail out if actor is dead at end of plan.
    let actor_at_end = match end_snap.unit(ctx.active.entity) {
        Some(u) if u.hp > 0 => u,
        _ => return 0.0,
    };

    // Max range across all abilities (mirrors snapshot build_snapshot logic,
    // but over all target types — we want "can I reach and hit anything?").
    let max_range: u32 = actor_at_end
        .abilities
        .iter()
        .filter_map(|id| ctx.world.content.abilities.get(id))
        .map(|def| def.range.max)
        .max()
        .unwrap_or(0);

    if max_range == 0 {
        return 0.0;
    }

    let reachable_enemies = end_snap
        .enemies_of(ctx.active.team)
        .filter(|e| e.hp > 0)
        .filter(|e| plan.final_pos.unsigned_distance_to(e.pos) <= max_range)
        .count();

    // Normalize: 0 = no targets in range, 1.0 = ≥3 targets.
    (reachable_enemies as f32 / 3.0).clamp(0.0, 1.0)
}

/// Count of living enemies within AoE-typical radius of the actor's end
/// position, normalised to [0, 1] (≥3 enemies → 1.0).
///
/// Only meaningful for actors tagged `HAS_AOE` — others return 0.0 because
/// cluster density is irrelevant without area coverage. Radius 2 is the
/// conservative baseline for the current AoE content (most cluster spells
/// use radius 1–2).
///
/// TODO(5.5/5.6): derive radius from the actor's actual AoE abilities rather
/// than the fixed constant once we have a reliable way to enumerate AoE
/// shapes from `AbilityDef.aoe`.
pub(crate) fn compute_density_value(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> f32 {
    // Density matters only for actors with AoE abilities.
    if !ctx.active.tags.contains(AiTags::HAS_AOE) {
        return 0.0;
    }

    let end_snap = plan.sim_snapshots.last().unwrap_or(initial_snap);

    // Conservative AoE radius baseline for existing content.
    let radius: u32 = 2;
    let count = end_snap
        .enemies_of(ctx.active.team)
        .filter(|e| e.hp > 0)
        .filter(|e| plan.final_pos.unsigned_distance_to(e.pos) <= radius)
        .count();

    // Normalize: 0 = no enemies in cluster range, 1.0 = ≥3 enemies.
    (count as f32 / 3.0).clamp(0.0, 1.0)
}

/// Signed change in ally-support map value between the actor's start and final
/// position, clamped to [−1, 1].
///
/// Positive → moved toward ally support (better tactical cohesion); negative →
/// moved away (isolation). Used to reward Support actors that reposition closer
/// to allies in need and to penalise Ranged actors drifting away from the line.
///
/// Signed axis — unlike the strictly-positive axes, this can contribute a
/// penalty in the aggregator if the weight is positive and the actor retreated.
pub(crate) fn compute_pressure_spacing_zone(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
    let support_at_end = ctx.maps.ally_support.get(plan.final_pos);
    let support_at_start = ctx.maps.ally_support.get(ctx.active.pos);
    (support_at_end - support_at_start).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::{AiTags, BattleSnapshot};
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::scoring::factors::TerminalFactor;
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
        assert_eq!(terminal.get(TerminalFactor::ExposureAtEnd), 0.0);
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
            (terminal.get(TerminalFactor::ExposureAtEnd) - 0.8).abs() < 1e-5,
            "expected ~0.8, got {}",
            terminal.get(TerminalFactor::ExposureAtEnd)
        );
    }

    /// `exposure_at_end` is non-zero when the plan's final position has danger > 0.
    /// Simulates: actor ends in an enemy-threat zone (danger map has value at that tile).
    ///
    /// Source verification: `compute_exposure_at_end` reads `ctx.maps.danger.get(plan.final_pos)`.
    /// When danger map is zero everywhere, result is 0.0. When danger > 0 at final_pos, result > 0.
    #[test]
    fn exposure_at_end_non_zero_when_actor_in_enemy_threat_zone() {
        let actor_pos = hex_from_offset(0, 0);
        let enemy_adjacent = hex_from_offset(1, 0); // actor will end at actor_pos in danger
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let mut maps = empty_maps();
        // Simulate enemy threat: danger at actor's final position (enemy adjacent).
        maps.danger.add(actor_pos, 0.6);
        let _ = enemy_adjacent; // used conceptually above
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let exposure = crate::combat::ai::planning::terminal::compute_exposure_at_end(&plan, &ctx);
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
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps(); // danger map all zeros — safe backline
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = idle_plan(actor_pos, snap.clone());
        let exposure = crate::combat::ai::planning::terminal::compute_exposure_at_end(&plan, &ctx);
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
        let snap = BattleSnapshot::new(vec![actor.clone(), far_enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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

    // ── line_actionability ────────────────────────────────────────────────

    #[test]
    fn line_actionability_zero_when_no_abilities() {
        // Actor with empty abilities vec → max_range = 0 → score = 0.
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build(); // abilities=[]
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let end_snap = BattleSnapshot::new(vec![actor_dead, enemy], 1);
        let actor_initial = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(&["melee_attack"])
            .build();
        let initial_snap = BattleSnapshot::new(vec![actor_initial.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone(), far_enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();

        // 1 enemy in range → ~0.33
        let snap1 = BattleSnapshot::new(vec![actor.clone(), e1.clone()], 1);
        let ctx1 = make_scoring_ctx(&world, &snap1, &maps, &reservations, &actor);
        let plan1 = idle_plan(actor_pos, snap1.clone());
        let score1 = compute_line_actionability(&plan1, &snap1, &ctx1);
        assert!(
            (score1 - 1.0 / 3.0).abs() < 0.01,
            "1 enemy in range → expected ~0.33, got {score1}"
        );

        // 3 enemies in range → 1.0 (clamped)
        let snap3 = BattleSnapshot::new(vec![actor.clone(), e1, e2, e3], 1);
        let ctx3 = make_scoring_ctx(&world, &snap3, &maps, &reservations, &actor);
        let plan3 = idle_plan(actor_pos, snap3.clone());
        let score3 = compute_line_actionability(&plan3, &snap3, &ctx3);
        assert_eq!(score3, 1.0, "3 enemies in range → expected 1.0, got {score3}");
    }

    // ── density_value ──────────────────────────────────────────────────────

    #[test]
    fn density_value_zero_for_non_aoe_actor() {
        // Actor without HAS_AOE tag → density_value = 0 regardless of enemies.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build(); // tags=empty
        let e1 = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let e2 = UnitBuilder::new(3, Team::Player, hex_from_offset(0, 1)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), e1, e2], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone(), far_enemy], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone(), e1, e2, e3], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
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
}
