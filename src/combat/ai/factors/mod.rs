//! 9-factor scoring pipeline.
//!
//! Takes a candidate pool and produces one composite score per candidate by
//! combining `[damage, kill, cc, heal, position, risk, focus, intent, scarcity]`
//! with role-axis weights, per-factor difficulty multipliers, and noise.
//!
//! `score_candidates` is the public entry. `compute_factors` is exposed so the
//! debug printer can display the raw per-factor breakdown for top-K candidates.
//!
//! Module layout:
//! - `offensive` — damage / heal / kill / cc (single-target and AoE), `aoe_area`.
//! - `scarcity`  — resource-vs-swing scoring for Cast candidates.
//! - `adjustments` — reservation nerfs + crit-fail expected-value adjustment.

#![allow(clippy::too_many_arguments)]

mod adjustments;
mod offensive;
mod scarcity;

pub use adjustments::crit_fail_adjusted;
pub use offensive::aoe_area;

use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{intent_score, TacticalIntent};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::estimate_st_damage;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{CasterContext, EffectDef};
use crate::core::modifier;
use crate::core::DiceRng;
use crate::game::components::Abilities;

// ── Factor layout ───────────────────────────────────────────────────────────

/// 9 utility factors: damage, kill, cc, heal, position, risk, focus, intent, scarcity.
pub const NUM_FACTORS: usize = 9;

/// Factors that can be negative (position, intent, scarcity).
/// These use symmetric normalization: divide by max(|min|, |max|) → [-1, 1].
/// Non-negative factors use standard max normalization → [0, 1].
const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, true, false, false, true, true,
];

/// Per-candidate offensive factors (populated only for Cast).
#[derive(Default)]
pub(super) struct OffensiveFactors {
    pub(super) damage: f32,
    pub(super) heal: f32,
    pub(super) kill: f32,
    pub(super) cc: f32,
}

// ── Top-level scoring ───────────────────────────────────────────────────────

pub fn score_candidates(
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> Vec<f32> {
    if candidates.is_empty() {
        return vec![];
    }

    // Compute raw factors for each candidate.
    let raw: Vec<[f32; NUM_FACTORS]> = candidates
        .iter()
        .map(|c| compute_factors(c, active, intent, ctx, snap, maps, reservations))
        .collect();

    // Find per-factor extremes for normalization.
    let mut maxes = [0.0f32; NUM_FACTORS];
    let mut mins = [0.0f32; NUM_FACTORS];
    for factors in &raw {
        for (i, &v) in factors.iter().enumerate() {
            if v > maxes[i] { maxes[i] = v; }
            if v < mins[i] { mins[i] = v; }
        }
    }

    // Compute normalization denominator per factor.
    let mut denom = [0.0f32; NUM_FACTORS];
    for i in 0..NUM_FACTORS {
        denom[i] = if SIGNED_FACTOR[i] {
            // Symmetric: divide by max absolute value → [-1, 1]
            mins[i].abs().max(maxes[i].abs())
        } else {
            // Non-negative: divide by max → [0, 1]
            maxes[i]
        };
    }

    // Normalize and apply composed axis weights, with per-factor difficulty multipliers.
    let mut weights = active.role.factor_weights();
    // intent factor (idx 7): scaled by intent_commitment.
    weights[7] *= ctx.difficulty.intent_commitment;
    // scarcity factor (idx 8): scaled by resource_discipline.
    weights[8] *= ctx.difficulty.resource_discipline;

    let noise_amp = ctx.difficulty.score_noise();

    raw.iter()
        .zip(candidates.iter())
        .map(|(factors, candidate)| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    factors[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }

            // Summon bonus bypasses normalization: the factor pipeline can't
            // see the strategic value of creating an ally, and for hybrid roles
            // the damage-axis weight is too low to lift a raw summon score.
            score += summon_bonus(candidate, active, ctx, snap);

            // Add noise.
            if noise_amp > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
                score += noise;
            }

            score
        })
        .collect()
}

/// Additive bonus applied post-normalization when the candidate is a Summon.
/// Valued as `summon_dpr × decay` — the factor pipeline can't see the
/// strategic value of creating an ally, and normalization would erase any
/// single-factor contribution.
fn summon_bonus(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let CandidateKind::Cast { ability, .. } = &candidate.kind else { return 0.0 };
    let Some(def) = ctx.content.abilities.get(ability) else { return 0.0 };
    let EffectDef::Summon { template, max_active } = &def.effect else { return 0.0 };

    let cap = max_active.unwrap_or(3).max(1) as f32;
    let count = snap
        .units
        .iter()
        .filter(|u| u.summoner == Some(active.entity))
        .count() as f32;
    let decay = (1.0 - (count / cap)).max(0.0);
    if decay <= 0.0 { return 0.0 }

    let Some(tpl) = ctx.content.unit_templates.get(template) else { return 0.0 };
    let weapon = ctx.content.weapons.get(&tpl.equipment.main_hand);
    let caster_ctx = CasterContext {
        str_mod: modifier(tpl.stats.strength),
        int_mod: modifier(tpl.stats.intelligence),
        spell_power: weapon.map_or(0, |wd| wd.spell_power),
        weapon_dice: weapon.map(|wd| wd.dice.clone()),
    };
    let abilities = Abilities(tpl.ability_ids.clone());
    let dpr = estimate_st_damage(&caster_ctx, &abilities, ctx.content);
    dpr * decay
}

/// Compute the 9 raw utility factors for a single candidate.
/// Axes: [damage, kill, cc, heal, position, risk, focus, intent, scarcity].
pub fn compute_factors(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> [f32; NUM_FACTORS] {
    let mut off = match &candidate.kind {
        CandidateKind::Cast { ability, target_pos, target } => {
            offensive::compute_offensive(ability, *target_pos, *target, candidate.tile, active, ctx, snap)
        }
        CandidateKind::MoveOnly => OffensiveFactors::default(),
    };

    let mut position = evaluate_position(candidate.tile, &active.role, maps);
    let risk = 1.0 - maps.danger.get(candidate.tile);
    let mut focus = candidate
        .target()
        .and_then(|t| snap.unit(t))
        .map(|t| target_priority(active, t, snap))
        .unwrap_or(0.0);
    let intent_val = intent_score(intent, candidate, active, snap, maps, ctx.content, ctx.difficulty);

    adjustments::apply_reservation_adjustments(candidate, &mut off, &mut focus, &mut position, snap, ctx, reservations);

    let scarcity = match &candidate.kind {
        CandidateKind::Cast { .. } => scarcity::compute_scarcity(candidate, active, off.kill, ctx, snap),
        CandidateKind::MoveOnly => 0.0,
    };

    [off.damage, off.kill, off.cc, off.heal, position, risk, focus, intent_val, scarcity]
}

#[cfg(test)]
mod tests {
    // ── Normalization tests ───────────────────────────────────────────

    #[test]
    fn signed_normalization_preserves_negative_order() {
        let values = [-3.0f32, -1.0, -0.5];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let normalized: Vec<f32> = values.iter().map(|v| v / max_abs).collect();
        assert_eq!(normalized, vec![-1.0, -1.0 / 3.0, -0.5 / 3.0]);
        assert!(normalized[0] < normalized[1]);
        assert!(normalized[1] < normalized[2]);
    }

    #[test]
    fn signed_normalization_flat_batch_gives_zero() {
        let values = [0.0f32; 3];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        for &v in &values {
            let norm = if max_abs > f32::EPSILON { v / max_abs } else { 0.0 };
            assert_eq!(norm, 0.0);
            assert!(!norm.is_nan());
        }
    }
}
