//! Terminal state evaluation — one-shot per-plan assessment of the final sim
//! snapshot. Captures "where we ended up", not "what we did along the way".
//! Eight axes, three clusters:
//! - Defensive: `exposure_at_end`, `next_turn_lethality`.
//! - Offensive: `secure_kill`, `ally_rescue`, `board_control_gain`.
//! - Geometric: `line_actionability`, `density_value`, `pressure_spacing_zone`.
//!
//! Per-axis `compute_*` helpers are `pub(crate)`, consumed by the
//! `factors::terminal` leaf modules.

use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::{FactorTerminalScore, TerminalFactor};
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::world::tags::AiTags;

/// Compute the terminal-state score (all 8 axes) for a plan from its final sim
/// snapshot.
pub fn terminal_state_score(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> FactorTerminalScore {
    let mut out = FactorTerminalScore::default();
    out.set(
        TerminalFactor::ExposureAtEnd,
        compute_exposure_at_end(plan, ctx),
    );
    out.set(
        TerminalFactor::NextTurnLethality,
        compute_next_turn_lethality(plan, initial_snap, ctx),
    );
    out.set(TerminalFactor::SecureKill, compute_secure_kill(plan));
    out.set(
        TerminalFactor::AllyRescue,
        compute_ally_rescue(plan, initial_snap, ctx),
    );
    out.set(
        TerminalFactor::BoardControlGain,
        compute_board_control_gain(plan, ctx),
    );
    out.set(
        TerminalFactor::LineActionability,
        compute_line_actionability(plan, initial_snap, ctx),
    );
    out.set(
        TerminalFactor::DensityValue,
        compute_density_value(plan, initial_snap, ctx),
    );
    out.set(
        TerminalFactor::PressureSpacingZone,
        compute_pressure_spacing_zone(plan, ctx),
    );
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
/// `p_kill_now` — confirmed kill from sim; `p_kill_soon` — DoT finishes next
/// round (half-weight reflects lower certainty).
///
/// # Overlap note
/// `factors::offensive` step factors also credit these kills, but apply a depth
/// discount (`base^k`). `secure_kill` is a flat roll-up — depth-insensitive, so
/// it values multi-step kill combos the discounted sum undervalues. Keep both;
/// double-counting is bounded by separately-tuned weight tables
/// (`axis_factor_weights` vs `axis_terminal_weights`).
pub(crate) fn compute_secure_kill(plan: &TurnPlan) -> f32 {
    plan.annotation
        .outcomes
        .iter()
        .map(|o| o.p_kill_now + 0.5 * o.p_kill_soon)
        .sum::<f32>()
        .min(1.0)
}

/// Credit for rescuing an endangered ally this turn.
///
/// "Endangered" = plan-start HP < 40% AND tile danger > 0.5. If by plan end the
/// ally is above 60% HP, credit `1 − initial_hp_pct` (proportional to severity).
/// Actor excluded (self covered by `exposure_at_end` / `next_turn_lethality`).
/// Clamped to [0, 1]. Thresholds (0.4, 0.5, 0.6) hard-coded pending `Thresholds`.
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
        let was_endangered =
            ally_initial.hp_pct() < 0.4 && ctx.maps.danger.get(ally_initial.pos) > 0.5;
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
        Some(u) if u.hp() > 0 => u.hp(),
        _ => return 0.0,
    };

    let final_pos = plan.final_pos;
    let dpr_sum: f32 = end_snap
        .enemies_of(ctx.active.team)
        .filter(|e| e.hp() > 0)
        .filter(|e| {
            let reach =
                (e.effective_speed().max(0) as u32).saturating_add(e.cache.max_attack_range);
            final_pos.unsigned_distance_to(e.pos) <= reach
        })
        .map(crate::combat::ai::scoring::horizon_avg)
        .sum();

    // lethality > 1.0 means "likely dead next turn"; clamp to [0, 1].
    (dpr_sum / actor_hp_at_end as f32).clamp(0.0, 1.0)
}

// ── Step 5.3: geometric cluster ───────────────────────────────────────────────

/// Enemies within max cast range of the actor's end position, normalised to
/// [0, 1] (≥3 → 1.0). Measures positioning to act next turn without moving.
///
/// Returns 0.0 if the actor is dead at end of plan or has no ranged abilities.
///
/// TODO(5.5): if abilities change mid-plan (e.g. summon expiry), re-derive from
/// end_snap. Current content has static per-turn abilities.
pub(crate) fn compute_line_actionability(
    plan: &TurnPlan,
    initial_snap: &BattleSnapshot,
    ctx: &ScoringCtx,
) -> f32 {
    let end_snap = plan.sim_snapshots.last().unwrap_or(initial_snap);

    // Bail out if actor is dead at end of plan.
    let actor_at_end = match end_snap.unit(ctx.active.entity()) {
        Some(u) if u.hp() > 0 => u,
        _ => return 0.0,
    };

    // Max range across all abilities (mirrors snapshot build_snapshot logic,
    // but over all target types — we want "can I reach and hit anything?").
    let max_range: u32 = actor_at_end
        .cache
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
        .filter(|e| e.hp() > 0)
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
        .filter(|e| e.hp() > 0)
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
