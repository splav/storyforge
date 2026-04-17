#![allow(clippy::too_many_arguments)]
use crate::combat::ai::candidates::generate_candidates;
pub use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::constraints::filter_candidates;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::factors::{aoe_area, compute_factors, score_candidates};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    default_focus_target, intent_score, intent_viability_threshold, select_intent, update_memory,
    AiMemory, TacticalIntent,
};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::{applies_cc, score_action};
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::content::abilities::{AoEShape, CasterContext, TargetType};
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{Abilities, Team};
use crate::game::hex::{has_los, in_bounds, Hex};
use crate::game::pathfinding::ReachableMap;
use crate::game::resources::GameDb;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// ── Public types ────────────────────────────────────────────────────────────

pub enum AiDecision {
    CastInPlace {
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveAndCast {
        path: Vec<Hex>,
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveCloser {
        path: Vec<Hex>,
    },
    MoveOnlyRetreat {
        path: Vec<Hex>,
    },
    EndTurn,
}

// ── Context ─────────────────────────────────────────────────────────────────

pub struct UtilityContext<'a> {
    pub db: &'a GameDb,
    pub difficulty: &'a DifficultyProfile,
    pub caster: &'a CasterContext,
    pub abilities: &'a Abilities,
    pub opponent_team: Team,
    pub crit_fail_effect: CritFailEffect,
    pub crit_fail_chance: f32,
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Top-level decision function. Replaces evaluate_targets + plan_movement.
/// When `debug` is true, returns an `AiDebugSnapshot` alongside the decision.
pub fn pick_action(
    actor: Entity,
    actor_pos: Hex,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
    rng: &mut DiceRng,
    memory: &mut AiMemory,
    reservations: &mut Reservations,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>) {
    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None);
    };

    // ── Select tactical intent ──────────────────────────────────────────
    let choice = select_intent(active, snap, maps, memory, ctx.difficulty);
    update_memory(memory, &choice.intent);
    let mut intent = choice.intent;
    let mut intent_reason = choice.reason;

    // ── Generate candidates ─────────────────────────────────────────────
    let mut candidates = generate_candidates(actor_pos, active, ctx, snap, maps, reach);

    if candidates.is_empty() {
        let decision = fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &intent_reason, &decision, "no candidates generated", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Hard constraints ────────────────────────────────────────────────
    filter_candidates(&mut candidates, active, snap, maps, ctx.db);

    if candidates.is_empty() {
        let decision = fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &intent_reason, &decision, "all filtered by constraints", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Utility scoring ─────────────────────────────────────────────────
    let mut scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, reservations, rng);

    // ── Intent viability guard ─────────────────────────────────────────
    // If the chosen intent can't be executed by any candidate (e.g., Reposition
    // with no tile actually improving, FocusTarget on an unreachable enemy),
    // fall back to FocusTarget over a reachable enemy and rescore.
    if let Some(threshold) = intent_viability_threshold(&intent) {
        let max_align = candidates
            .iter()
            .map(|c| intent_score(&intent, c, active, snap, maps, ctx.db, ctx.difficulty))
            .fold(f32::NEG_INFINITY, f32::max);
        if max_align < threshold {
            // Skip the original target if it was an unreachable FocusTarget —
            // otherwise the "fallback" picks the same target and does nothing.
            let exclude = match &intent {
                TacticalIntent::FocusTarget { target } => Some(*target),
                _ => None,
            };
            if let Some(default_target) = default_focus_target(active, snap, &candidates, exclude) {
                let new_intent = TacticalIntent::FocusTarget { target: default_target };
                // Only log + rescore if something actually changed.
                if intent.kind() != new_intent.kind() || intent.target() != new_intent.target() {
                    let original_label = format!("{:?}", intent.kind());
                    // Rebuild reason with the real numbers that made the guard fire —
                    // no separate "explain" path to drift from this.
                    intent_reason = format!(
                        "fallback from {}: max_align={:.2}<threshold={:.2}",
                        original_label, max_align, threshold,
                    );
                    intent = new_intent;
                    scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, reservations, rng);
                }
            }
        }
    }

    // ── Sanity check on top candidates ─────────────────────────────────
    sanity_adjust(&mut scored, &candidates, active, snap, maps, ctx);

    // ── ProtectSelf: mask non-defensive candidates so pick picks safety ──
    // Retreat is already a first-class MoveOnly candidate in the pool, so no
    // separate retreat branch — the top candidate after masking is either a
    // defensive cast (self-heal) or a safe MoveOnly tile.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let current_danger = maps.danger.get(active.pos);
        let def_margin = ctx.difficulty.defensive_tile_margin();
        let mut any_defensive = false;
        for (i, s) in scored.iter_mut().enumerate() {
            if is_defensive(&candidates[i], current_danger, ctx.db, maps, def_margin) {
                any_defensive = true;
            } else {
                *s = f32::NEG_INFINITY;
            }
        }
        // No viable survival option → LastStand: re-score for maximum impact.
        if !any_defensive {
            let last_stand = TacticalIntent::LastStand;
            scored = score_candidates(&candidates, active, &last_stand, ctx, snap, maps, reservations, rng);
        }
    }

    // Pick best: combine mercy (soft shift toward gentler options) and
    // top_k (random pick among top-K, controlled by decision_quality).
    let (best_idx, pick_mech) = pick_best_candidate(
        &scored, &candidates, active, &intent, ctx, snap, maps, reservations, rng,
    );

    // Build debug snapshot before swap_remove invalidates indices.
    let debug_snapshot = if debug {
        let best = &candidates[best_idx];
        let decision_preview = decision_from_candidate(best, actor, actor_pos);
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &intent_reason, &candidates, &scored, &decision_preview,
            ctx, snap, maps, reservations, debug_names, Some(&pick_mech),
        ))
    } else {
        None
    };

    // ── Record reservations for subsequent units ─────────────────────
    {
        let best = &candidates[best_idx];
        record_reservation(best, active, ctx, snap, reservations, actor_pos);
    }

    let best = candidates.swap_remove(best_idx);
    let decision = decision_from_candidate(&best, actor, actor_pos);

    (decision, debug_snapshot)
}

/// Convert a scored candidate into the corresponding AiDecision.
/// AoE candidates (`target == None`) use `actor` as the engine sentinel —
/// the `UseAbility` contract requires an Entity field even for area casts.
fn decision_from_candidate(c: &ActionCandidate, actor: Entity, actor_pos: Hex) -> AiDecision {
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


// ── Utility scoring ─────────────────────────────────────────────────────────


// ── Sanity check ────────────────────────────────────────────────────────────

/// Post-score verification on the top-3 candidates. Applies multiplicative
/// penalties for dangerous situations that per-factor scoring can't catch.
fn sanity_adjust(
    scores: &mut [f32],
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    ctx: &UtilityContext,
) {
    if scores.len() <= 1 {
        return;
    }

    // Find top-3 indices by score.
    let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(3);

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap.allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    let occupied: HashSet<Hex> = snap.units.iter().map(|u| u.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, maps);
    let current_danger = maps.danger.get(active.pos);

    for (idx, _) in &indexed {
        let c = &candidates[*idx];
        let mut penalty = 1.0f32;

        // 1. Survival: quadratic penalty on (low HP × dangerous tile).
        // Replaces the old step penalty (×0.3 / ×0.6) and the constraint-level
        // "don't walk into death" hard filter — hard cuts left the AI with no
        // retreat option when every reachable tile was dangerous. Gradient
        // lets retreat candidates reach scoring and compete; heal usually
        // still wins when available, retreat wins when nothing else does.
        //
        //   penalty_frac = LOW_HP_FACTOR × hp_need × max(0, danger − 0.5)²
        //   hp_need      = clamp((0.6 − hp_pct) / 0.6, 0, 1)
        //   score        *= (1 − penalty_frac).max(0.25)  // floor to stay comparable
        const LOW_HP_FACTOR: f32 = 1.2;
        let danger_frac = maps.danger.get(c.tile);
        let hp_fraction = active.hp_pct();
        let hp_need = ((0.6 - hp_fraction) / 0.6).clamp(0.0, 1.0);
        let excess = (danger_frac - 0.5).max(0.0);
        let penalty_frac = LOW_HP_FACTOR * hp_need * excess * excess;
        if penalty_frac > 0.0 {
            penalty *= (1.0 - penalty_frac).max(0.25);
        }

        // 2. Healer exposure: are we abandoning an allied healer?
        // Healer exposure check: if the actor isn't itself a significant
        // healer (Support axis < 0.3), it shouldn't abandon the team healer.
        if active.role.support < 0.3 {
            for ally in &allies {
                if !ally.tags.contains(AiTags::CAN_HEAL) {
                    continue;
                }
                let was_near = active.pos.unsigned_distance_to(ally.pos) <= 1;
                let will_be_far = c.tile.unsigned_distance_to(ally.pos) > 2;
                if was_near && will_be_far {
                    let other_guard = allies.iter().any(|a| {
                        a.entity != ally.entity && a.pos.unsigned_distance_to(ally.pos) <= 2
                    });
                    if !other_guard {
                        penalty *= 0.5;
                    }
                }
            }
        }

        // 3. LOS check: ranged unit moving to a blind spot.
        if active.tags.contains(AiTags::RANGED) && !enemies.is_empty() {
            let can_see_any = enemies.iter().any(|e| {
                has_los(c.tile, e.pos, |mid| {
                    occupied.contains(&mid) && mid != c.tile && mid != e.pos
                })
            });
            if !can_see_any {
                penalty *= 0.3;
            }
        }

        // 4. Retreat trap: tile with very few unblocked neighbors.
        let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
        let open_neighbors = c.tile.all_neighbors().iter()
            .filter(|&&n| in_bounds(n) && !ally_positions.contains(&n))
            .count();
        if open_neighbors < 2 {
            penalty *= 0.5;
        }

        // 5. Self-AoE: heavy penalty for friendly_fire AoE that hits caster.
        if let CandidateKind::Cast { ability, target_pos, .. } = &c.kind {
            if let Some(def) = ctx.db.abilities.get(ability) {
                if def.friendly_fire && def.aoe != AoEShape::None {
                    let area = aoe_area(def, *target_pos, c.tile);
                    if area.contains(&c.tile) {
                        penalty *= 0.5;
                    }
                }
            }
        }

        // 6. Synergy bonus: candidate that MOVES to a better tile AND casts a
        // useful ability — the "retreat-and-help" combo. Multiplicative so it
        // doesn't flip sign and scales with base score magnitude.
        if c.tile != active.pos {
            let safer_tile = maps.danger.get(c.tile) + 0.05 < current_danger;
            let better_pos = evaluate_position(c.tile, &active.role, maps) > current_pos_eval;
            let useful_cast = match &c.kind {
                CandidateKind::Cast { ability, .. } => {
                    ctx.db.abilities.get(ability).is_some_and(|def| {
                        def.effect.calc(ctx.caster).is_some() || !def.statuses.is_empty()
                    })
                }
                CandidateKind::MoveOnly => false,
            };
            if (safer_tile || better_pos) && useful_cast {
                penalty *= 1.1;
            }
        }

        scores[*idx] *= penalty;
    }
}

// ── Final pick: top-K sampling + mercy tie-breaker ─────────────────────────

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

fn pick_best_candidate(
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

// ── Retreat scoring ─────────────────────────────────────────────────────────

// ── Fallback ────────────────────────────────────────────────────────────────

/// When no attack candidates exist, move closer to enemies —
/// or retreat to the safest tile if LOW_HP.
fn fallback_move(
    _actor_pos: Hex,
    active: &UnitSnapshot,
    _ctx: &UtilityContext,
    snap: &BattleSnapshot,
    reach: &ReachableMap,
    maps: &InfluenceMaps,
) -> AiDecision {
    if !active.movement {
        return AiDecision::EndTurn;
    }

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    if enemies.is_empty() {
        return AiDecision::EndTurn;
    }

    // LOW_HP: retreat to the tile with lowest danger.
    if active.tags.contains(AiTags::LOW_HP) {
        let safest = reach
            .destinations
            .iter()
            .min_by(|a, b| {
                maps.danger
                    .get(**a)
                    .partial_cmp(&maps.danger.get(**b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        if let Some(dest) = safest {
            if let Some(path) = reach.path_to(dest) {
                if !path.is_empty() {
                    return AiDecision::MoveCloser { path };
                }
            }
        }
        return AiDecision::EndTurn;
    }

    // Normal: find reachable tile closest to any enemy.
    let mut best: Option<(Hex, u32)> = None;
    for &cell in &reach.destinations {
        for enemy in &enemies {
            let dist = cell.unsigned_distance_to(enemy.pos);
            if best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((cell, dist));
            }
        }
    }

    if let Some((dest, _)) = best {
        if let Some(path) = reach.path_to(dest) {
            return AiDecision::MoveCloser { path };
        }
    }

    AiDecision::EndTurn
}

// ── Reservation recording ───────────────────────────────────────────────────

fn record_reservation(
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
        if let Some(def) = ctx.db.abilities.get(ability) {
            let is_cc = applies_cc(def, ctx.db);
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
                        let dmg = score_action(def, target_unit, ctx.caster, ctx.db);
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

// ── Defensive classification ────────────────────────────────────────────────

/// A candidate is defensive if it heals/buffs self/ally, is pure movement to a
/// safer tile, OR an offensive action from a safer tile.
fn is_defensive(
    c: &ActionCandidate,
    current_danger: f32,
    db: &GameDb,
    maps: &InfluenceMaps,
    margin: f32,
) -> bool {
    // MoveOnly is defensive when moving to a safer tile.
    if c.is_move_only() {
        return maps.danger.get(c.tile) + margin < current_danger;
    }
    if let Some(ability) = c.ability() {
        if let Some(def) = db.abilities.get(ability) {
            if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                return true;
            }
        }
    }
    // Cast from a meaningfully safer tile also counts as defensive.
    maps.danger.get(c.tile) + margin < current_danger
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(id).expect("valid entity id"),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action: true,
            movement: true,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec!["melee_attack".into()],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        let active = units[0].entity;
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    fn empty_maps() -> InfluenceMaps {
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    // ── Sanity check tests ──────────────────────────────────────────────
    // (diverse_tiles_* live in candidates.rs; scarcity_* and normalization
    // tests live in factors.rs — each next to the code they cover.)

    fn cast(tile: Hex, ability: &str, target_pos: Hex, target: Entity) -> ActionCandidate {
        ActionCandidate {
            tile,
            path: vec![],
            kind: CandidateKind::Cast {
                ability: ability.into(),
                target_pos,
                target: Some(target),
            },
        }
    }

    fn candidate(tile: Hex, target: Entity) -> ActionCandidate {
        cast(tile, "melee_attack", tile, target)
    }

    #[test]
    fn sanity_penalizes_suicide_tile() {
        let dangerous = hex_from_offset(3, 3);
        let safe_tile = hex_from_offset(5, 4);
        let mut active = unit(0, Team::Enemy, hex_from_offset(4, 3));
        active.hp = 5; // low HP so survival check triggers
        let enemy = unit(1, Team::Player, hex_from_offset(2, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);

        let mut maps = empty_maps();
        // Normalized danger: 0.9 = very dangerous, 0.1 = safe.
        maps.danger.add(dangerous, 0.9);
        maps.danger.add(safe_tile, 0.1);

        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { db: &db, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(dangerous, enemy.entity),
            candidate(safe_tile, enemy.entity),
        ];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        assert!(
            scores[0] < scores[1],
            "dangerous tile ({:.1}) should score lower than safe ({:.1}) after sanity",
            scores[0], scores[1],
        );
    }

    #[test]
    fn sanity_preserves_safe_candidate() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(2, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);

        let maps = empty_maps(); // no danger anywhere
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { db: &db, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(tile, enemy.entity),
            candidate(hex_from_offset(3, 3), enemy.entity),
        ];
        let mut scores = vec![10.0, 8.0];
        let original = scores.clone();

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        // First candidate (safe tile, no danger) should keep full score.
        assert_eq!(scores[0], original[0], "safe candidate score should be unchanged");
    }

    #[test]
    fn sanity_ranged_penalizes_blind_spot() {
        let actor_pos = hex_from_offset(4, 3);
        let behind_wall = hex_from_offset(0, 0);
        let mut active = unit(0, Team::Enemy, actor_pos);
        active.tags = AiTags::RANGED;
        let enemy = unit(1, Team::Player, hex_from_offset(4, 1));

        // Place a blocker between (0,0) and (4,1) — any unit on the line.
        let blocker = unit(2, Team::Enemy, hex_from_offset(2, 1));
        let s = snap(vec![active.clone(), enemy.clone(), blocker]);

        let maps = empty_maps();
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { db: &db, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(behind_wall, enemy.entity),
            candidate(actor_pos, enemy.entity), // stay — has LOS
        ];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        // The blind-spot tile should be penalized.
        assert!(
            scores[0] < 10.0,
            "blind-spot tile should be penalized, got {:.1}",
            scores[0],
        );
    }

    #[test]
    fn sanity_penalizes_self_aoe() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(4, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);
        let maps = empty_maps();
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 3, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = UtilityContext { db: &db, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        // thunderstrike AoE circle r=1 centered on caster's own tile → self-hit.
        let self_aoe = cast(tile, "thunderstrike", tile, enemy.entity);
        let safe = candidate(tile, enemy.entity); // melee_attack, no AoE

        let candidates = vec![self_aoe, safe];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        assert!(
            scores[0] < 10.0,
            "self-AoE should be penalized, got {:.1}",
            scores[0],
        );
        assert!(
            scores[0] < scores[1],
            "self-AoE ({:.1}) should score lower than safe ({:.1})",
            scores[0], scores[1],
        );
    }

}
