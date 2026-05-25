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
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::world::tags::AiTags;
use crate::combat::ai::orchestration::ScoringCtx;

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
        if ally_initial.entity() == ctx.active.entity() {
            continue;
        }
        let was_endangered = ally_initial.hp_pct() < 0.4
            && ctx.maps.danger.get(ally_initial.pos) > 0.5;
        if !was_endangered {
            continue;
        }
        if let Some(ally_end) = end_snap.unit(ally_initial.entity()) {
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
    let actor_id = ctx.active.entity();

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
            let reach = (e.speed.max(0) as u32).saturating_add(e.cache.max_attack_range);
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
    let actor_at_end = match end_snap.unit(ctx.active.entity()) {
        Some(u) if u.hp > 0 => u,
        _ => return 0.0,
    };

    // Max range across all abilities (mirrors snapshot build_snapshot logic,
    // but over all target types — we want "can I reach and hit anything?").
    let max_range: u32 = actor_at_end
        .cache.abilities
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
    if !ctx.active.cache.tags.contains(AiTags::HAS_AOE) {
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
#[path = "terminal_state_tests.rs"]
mod tests;
