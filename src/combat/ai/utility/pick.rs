//! Legacy single-candidate picker — replaced by `planning::picker` but kept as
//! a safety net until Phase 4 cleanup. Its helpers are mirrored 1:1 on the
//! plan side and will be deleted once the plan flow is proven in-game.

#![allow(clippy::too_many_arguments, dead_code)]

use super::{AiDecision, UtilityContext};
use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::factors::{aoe_area, compute_factors};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::{applies_cc, score_action};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::DiceRng;
use crate::game::hex::Hex;
use bevy::prelude::*;

/// Convert a scored candidate into the corresponding AiDecision.
/// AoE candidates (`target == None`) use `actor` as the engine sentinel —
/// the `UseAbility` contract requires an Entity field even for area casts.
pub(super) fn decision_from_candidate(
    c: &ActionCandidate,
    actor: Entity,
    actor_pos: Hex,
) -> AiDecision {
    match &c.kind {
        CandidateKind::Cast { ability, target, target_pos } => {
            let target_ent = target.unwrap_or(actor);
            if c.tile == actor_pos {
                AiDecision::CastInPlace {
                    ability: ability.clone(),
                    target: target_ent,
                    target_pos: *target_pos,
                }
            } else {
                AiDecision::MoveAndCast {
                    path: c.path.clone(),
                    ability: ability.clone(),
                    target: target_ent,
                    target_pos: *target_pos,
                }
            }
        }
        CandidateKind::MoveOnly => {
            if c.path.is_empty() {
                AiDecision::EndTurn
            } else {
                AiDecision::MoveOnlyRetreat { path: c.path.clone() }
            }
        }
    }
}

/// Approximate "harshness" of a candidate for the mercy tie-breaker.
/// Finishing blows feel far more oppressive than CC, so kill dominates:
/// kill ∈ {0,1}, cc contribution capped at 0.5 regardless of raw magnitude.
fn mercy_cruelty(
    c: &ActionCandidate,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> f32 {
    let f = compute_factors(c, active, intent, ctx, snap, maps, reservations);
    // factors: [dmg, kill, cc, heal, pos, risk, focus, intent, scarcity]
    f[1] + (f[2] * 0.1).min(0.5)
}

/// Raw mechanics output from pick_best_candidate. Caller converts pool indices
/// to labels for human-readable debug.
pub struct PickMechanics {
    pub top_k: usize,
    pub window: f32,
    pub mercy_margin: f32,
    pub mercy_applied: bool,
    /// (candidate_index, final_score) in pool order.
    pub pool: Vec<(usize, f32)>,
    pub chosen_pos: usize,
}

pub(super) fn pick_best_candidate(
    scored: &[f32],
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> (usize, PickMechanics) {
    let top_k_req = ctx.difficulty.top_k_choice();
    let m = ctx.difficulty.mercy_margin();
    // Similarity window: candidates within `noise × 2` of the best are treated
    // as indistinguishable by the AI's noisy perception. Those clearly worse
    // are never sampled, even if top_k_choice > 1.
    let window = (ctx.difficulty.score_noise() * 2.0).max(0.05);

    if scored.is_empty() {
        return (
            0,
            PickMechanics {
                top_k: top_k_req,
                window,
                mercy_margin: m,
                mercy_applied: false,
                pool: vec![],
                chosen_pos: 0,
            },
        );
    }

    // Rank by raw score (descending).
    let mut ranked: Vec<(usize, f32)> = scored.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Apply mercy only inside the window of near-best candidates. A lethal move
    // that's clearly better than the alternatives stays outside the window and
    // is never devalued — mercy is a tie-breaker, not a blanket penalty.
    let best_score = ranked[0].1;
    let mut mercy_applied = false;
    if m > 0.0 && best_score.is_finite() {
        let mercy_end = ranked
            .iter()
            .position(|(_, s)| !s.is_finite() || *s < best_score - m)
            .unwrap_or(ranked.len());
        if mercy_end > 1 {
            let mut windowed: Vec<(usize, f32)> = ranked[..mercy_end]
                .iter()
                .map(|&(i, s)| {
                    let cruel =
                        mercy_cruelty(&candidates[i], active, intent, ctx, snap, maps, reservations);
                    (i, s - m * cruel)
                })
                .collect();
            windowed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (slot, item) in windowed.into_iter().enumerate() {
                ranked[slot] = item;
            }
            mercy_applied = true;
        }
    }

    // top_k sampling restricted to the similarity window — random pick only
    // among candidates whose score is within `window` of the best.
    let k = top_k_req.max(1).min(ranked.len());
    let best_after = ranked[0].1;
    let pool: Vec<(usize, f32)> = ranked
        .iter()
        .take(k)
        .filter(|(_, s)| s.is_finite() && *s >= best_after - window)
        .map(|&(i, s)| (i, s))
        .collect();

    if pool.is_empty() {
        // Fallback: all -inf (masked) — just return overall argmax.
        return (
            ranked[0].0,
            PickMechanics {
                top_k: k,
                window,
                mercy_margin: m,
                mercy_applied,
                pool: vec![(ranked[0].0, ranked[0].1)],
                chosen_pos: 0,
            },
        );
    }
    let chosen_pos = if pool.len() == 1 {
        0
    } else {
        (rng.roll_d(pool.len() as u32) as usize).saturating_sub(1)
    };
    (
        pool[chosen_pos].0,
        PickMechanics {
            top_k: k,
            window,
            mercy_margin: m,
            mercy_applied,
            pool,
            chosen_pos,
        },
    )
}

/// After picking the best candidate, reserve its expected damage/CC/tile so
/// subsequent AI units in the same turn avoid overkill and duplicate CC.
pub(super) fn record_reservation(
    best: &ActionCandidate,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    reservations: &mut Reservations,
    actor_pos: Hex,
) {
    // Enumerate hit enemies: single-target uses the declared target; AoE walks
    // the area. Both paths reserve damage/CC per enemy so subsequent AI units
    // avoid overkill and duplicate CC. Heals (SingleAlly) skip damage reservation.
    if let CandidateKind::Cast { ability, target_pos, target } = &best.kind {
        if let Some(def) = ctx.content.abilities.get(ability) {
            let is_cc = applies_cc(def, ctx.content);
            let hits: Vec<Entity> = if def.aoe == AoEShape::None {
                target.iter().copied().collect()
            } else {
                let area = aoe_area(def, *target_pos, best.tile);
                snap.enemies_of(active.team)
                    .filter(|e| area.contains(&e.pos))
                    .map(|e| e.entity)
                    .collect()
            };
            for ent in hits {
                if let Some(target_unit) = snap.unit(ent) {
                    if def.target_type != TargetType::SingleAlly {
                        let dmg = score_action(def, target_unit, ctx.caster, ctx.content);
                        if dmg > 0.0 {
                            reservations.reserve_damage(ent, dmg);
                        }
                    }
                    if is_cc {
                        reservations.reserve_cc(ent);
                    }
                }
            }
        }
    }

    // Record destination tile (applies to both Cast and MoveOnly).
    if best.tile != actor_pos {
        reservations.reserve_tile(best.tile);
    }
}
