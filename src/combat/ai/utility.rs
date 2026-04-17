#![allow(clippy::too_many_arguments)]
use crate::combat::ai::constraints::filter_candidates;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    default_focus_target, intent_score, intent_viability_threshold, select_intent, update_memory,
    AiMemory, TacticalIntent,
};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::scoring::{score_action, TargetInfo};
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::content::abilities::{AoEShape, CasterContext, TargetType};
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng, ResourceKind};
use crate::game::components::{Abilities, Team};
use crate::game::hex::{has_los, hex_circle, hex_line, in_bounds, Hex};
use crate::game::pathfinding::ReachableMap;
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;
use hexx::EdgeDirection;
use crate::combat::ai::debug::{
    ActorDebug, AiDebugSnapshot, CandidateDebug, DecisionDebug, IntentReasoning, PickDebug,
    PoolEntry, TileInfluence,
};
use crate::game::hex::hex_to_offset;
use std::collections::{HashMap, HashSet};

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ActionCandidate {
    pub tile: Hex,
    pub path: Vec<Hex>,
    pub kind: CandidateKind,
}

/// A candidate is either a cast (ability + target) or a pure movement to a
/// defensive tile. MoveOnly integrates "just retreat" into the normal scoring
/// pipeline — top_k, mercy, noise and intent all apply uniformly.
#[derive(Clone)]
pub enum CandidateKind {
    Cast {
        ability: AbilityId,
        target_pos: Hex,
        target: Entity,
    },
    MoveOnly,
}

impl ActionCandidate {
    pub fn ability(&self) -> Option<&AbilityId> {
        match &self.kind {
            CandidateKind::Cast { ability, .. } => Some(ability),
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn target(&self) -> Option<Entity> {
        match &self.kind {
            CandidateKind::Cast { target, .. } => Some(*target),
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn target_pos(&self) -> Option<Hex> {
        match &self.kind {
            CandidateKind::Cast { target_pos, .. } => Some(*target_pos),
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn is_move_only(&self) -> bool {
        matches!(self.kind, CandidateKind::MoveOnly)
    }
}

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

// ── Role weight tables ──────────────────────────────────────────────────────

/// 9 utility factors: damage, kill, cc, heal, position, risk, focus, intent, scarcity.
const NUM_FACTORS: usize = 9;

/// Factors that can be negative (position, intent, scarcity).
/// These use symmetric normalization: divide by max(|min|, |max|) → [-1, 1].
/// Non-negative factors use standard max normalization → [0, 1].
const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, true, false, false, true, true,
];

// Factor weights are no longer per-enum — they're composed from the unit's
// AxisProfile via `profile.factor_weights()`. See role.rs for axis definitions.

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
    positions: &HexPositions,
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
    let intent = select_intent(active, snap, maps, memory, ctx.difficulty);
    update_memory(memory, &intent);

    // ── Generate candidates ─────────────────────────────────────────────
    let mut candidates = generate_candidates(actor_pos, active, ctx, snap, maps, positions, reach);

    if candidates.is_empty() {
        let decision = fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &decision, "no candidates generated", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Hard constraints ────────────────────────────────────────────────
    filter_candidates(&mut candidates, active, snap, maps, ctx.db);

    if candidates.is_empty() {
        let decision = fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &decision, "all filtered by constraints", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Utility scoring ─────────────────────────────────────────────────
    let mut scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, reservations, rng);

    // ── Intent viability guard ─────────────────────────────────────────
    // If the chosen intent can't be executed by any candidate (e.g., Reposition
    // with no tile actually improving, FocusTarget on an unreachable enemy),
    // fall back to FocusTarget over a reachable enemy and rescore.
    let mut intent = intent;
    let mut intent_fallback: Option<(String, f32, f32)> = None;
    let threshold = intent_viability_threshold(&intent);
    if threshold.is_finite() {
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
                    intent_fallback = Some((original_label, max_align, threshold));
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
        let decision_preview = decision_from_candidate(best, actor_pos);
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &candidates, &scored, &decision_preview,
            ctx, snap, maps, reservations, debug_names, Some(&pick_mech),
            intent_fallback.as_ref(),
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
    let decision = decision_from_candidate(&best, actor_pos);

    (decision, debug_snapshot)
}

/// Convert a scored candidate into the corresponding AiDecision.
fn decision_from_candidate(c: &ActionCandidate, actor_pos: Hex) -> AiDecision {
    match &c.kind {
        CandidateKind::Cast { ability, target, target_pos } => {
            if c.tile == actor_pos {
                AiDecision::CastInPlace {
                    ability: ability.clone(),
                    target: *target,
                    target_pos: *target_pos,
                }
            } else {
                AiDecision::MoveAndCast {
                    path: c.path.clone(),
                    ability: ability.clone(),
                    target: *target,
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

// ── Candidate generation ────────────────────────────────────────────────────

fn generate_candidates(
    actor_pos: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    positions: &HexPositions,
    reach: &ReachableMap,
) -> Vec<ActionCandidate> {
    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();

    let tiles = select_diverse_tiles(actor_pos, active, ctx, snap, maps, reach, &enemies);
    // Include self — SingleAlly abilities (e.g. heal) should be self-castable.
    // Overheal/role constraints still filter out unneeded casts.
    let allies: Vec<&UnitSnapshot> = snap.allies_of(active.team).collect();

    let mut candidates = Vec::new();

    for &tile in &tiles {
        let path = if tile == actor_pos {
            vec![]
        } else {
            match reach.path_to(tile) {
                Some(p) => p,
                None => continue,
            }
        };

        // Needs movement to get there but doesn't have it.
        if !path.is_empty() && !active.movement {
            continue;
        }

        for ability_id in &ctx.abilities.0 {
            let Some(def) = ctx.db.abilities.get(ability_id) else { continue };

            // Check affordability.
            if !can_afford_snap(def, active) {
                continue;
            }

            // Need action point to cast.
            if !active.action {
                continue;
            }

            let max_range = def.range.max;

            // Generate target positions for this ability from this tile.
            let target_positions: Vec<Hex> = match def.aoe {
                AoEShape::None => match def.target_type {
                    TargetType::SingleEnemy => {
                        enemies
                            .iter()
                            .map(|e| e.pos)
                            .filter(|&p| max_range == 0 || tile.unsigned_distance_to(p) <= max_range)
                            .collect()
                    }
                    TargetType::SingleAlly => {
                        allies
                            .iter()
                            .map(|a| a.pos)
                            .filter(|&p| max_range == 0 || tile.unsigned_distance_to(p) <= max_range)
                            .collect()
                    }
                    TargetType::Myself => continue,
                },
                AoEShape::Circle { radius } => {
                    let mut centers: HashSet<Hex> = HashSet::new();
                    for enemy in &enemies {
                        for cell in hex_circle(enemy.pos, radius) {
                            if max_range == 0 || tile.unsigned_distance_to(cell) <= max_range {
                                centers.insert(cell);
                            }
                        }
                    }
                    centers.into_iter().collect()
                }
                AoEShape::Line { .. } => {
                    let effective_range = if max_range == 0 { 1 } else { max_range };
                    let mut results = Vec::new();
                    for dir in EdgeDirection::ALL_DIRECTIONS {
                        let step: Hex = dir.into();
                        for d in 1..=effective_range as i32 {
                            let pos = tile + step * d;
                            if !in_bounds(pos) {
                                break;
                            }
                            results.push(pos);
                        }
                    }
                    results
                }
            };

            for target_pos in target_positions {
                // Filter stale dead entities: positions doesn't remove on
                // death, so entity_at can return a corpse. snap.unit() only
                // returns living units, so we gate on that.
                let target_entity = positions
                    .entity_at(target_pos)
                    .filter(|e| snap.unit(*e).is_some())
                    .unwrap_or(active.entity);

                candidates.push(ActionCandidate {
                    tile,
                    path: path.clone(),
                    kind: CandidateKind::Cast {
                        ability: ability_id.clone(),
                        target_pos,
                        target: target_entity,
                    },
                });
            }
        }
    }

    // MoveOnly: add pure-movement options to safe reachable tiles. These let
    // the AI choose retreat via the normal scoring pipeline (with noise, top_k,
    // mercy) instead of a special-case branch.
    if active.movement {
        add_move_only_candidates(actor_pos, reach, maps, &mut candidates);
    }

    // Deduplicate by (ability, target) for Cast, by tile for MoveOnly —
    // keeping the shortest path in each bucket.
    candidates.sort_by(|a, b| a.path.len().cmp(&b.path.len()));
    let mut seen_cast: HashSet<(AbilityId, Entity)> = HashSet::new();
    let mut seen_move: HashSet<Hex> = HashSet::new();
    candidates.retain(|c| match &c.kind {
        CandidateKind::Cast { ability, target, .. } => {
            seen_cast.insert((ability.clone(), *target))
        }
        CandidateKind::MoveOnly => seen_move.insert(c.tile),
    });

    // Priority-aware ordering: sort by (target_priority DESC, path_len ASC).
    // High-priority targets survive budget truncation even on crowded fields;
    // within the same target, shortest path wins.
    let priority_of = |c: &ActionCandidate| -> f32 {
        c.target()
            .and_then(|t| snap.unit(t))
            .filter(|u| u.team != active.team) // allies use team-neutral priority
            .map(|u| target_priority(active, u, snap))
            .unwrap_or(0.0)
    };
    candidates.sort_by(|a, b| {
        let pa = priority_of(a);
        let pb = priority_of(b);
        pb.partial_cmp(&pa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.len().cmp(&b.path.len()))
    });

    // Per-target guarantee: make sure the shortest-path candidate for every
    // alive enemy survives truncation. Otherwise budget-cap can erase a whole
    // "how to reach X" column, making the AI believe X is untargetable.
    let budget = ctx.difficulty.candidate_budget.max(1);
    if candidates.len() > budget {
        let mut pinned: Vec<usize> = Vec::new();
        let mut seen_targets: HashSet<Entity> = HashSet::new();
        for (i, c) in candidates.iter().enumerate() {
            if let Some(target) = c.target() {
                if seen_targets.insert(target) {
                    pinned.push(i);
                }
            }
        }
        let mut kept: Vec<ActionCandidate> = Vec::with_capacity(budget);
        let mut pinned_set: HashSet<usize> = pinned.iter().copied().collect();
        // 1. Pinned candidates first (one per target).
        for &i in &pinned {
            if kept.len() < budget {
                kept.push(candidates[i].clone());
            }
        }
        // 2. Fill remaining slots with the rest in priority order.
        for (i, c) in candidates.iter().enumerate() {
            if kept.len() >= budget { break; }
            if !pinned_set.remove(&i) {
                kept.push(c.clone());
            }
        }
        candidates = kept;
    }

    candidates
}

/// Pick top-3 safe reachable tiles by escape map and add MoveOnly candidates.
fn add_move_only_candidates(
    actor_pos: Hex,
    reach: &ReachableMap,
    maps: &InfluenceMaps,
    out: &mut Vec<ActionCandidate>,
) {
    let mut scored: Vec<(Hex, f32)> = reach
        .destinations
        .iter()
        .filter(|&&h| h != actor_pos)
        .map(|&h| (h, maps.escape.get(h)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (tile, _) in scored.into_iter().take(3) {
        let Some(path) = reach.path_to(tile) else { continue };
        if path.is_empty() {
            continue;
        }
        out.push(ActionCandidate { tile, path, kind: CandidateKind::MoveOnly });
    }
}

// ── Diverse tile selection ──────────────────────────────────────────────────

/// Pick top-N tiles from `reach.destinations` scored by `f`, insert into `out`.
fn pick_top(
    reach: &ReachableMap,
    n: usize,
    out: &mut HashSet<Hex>,
    f: impl Fn(Hex) -> f32,
) {
    let mut scored: Vec<(Hex, f32)> = reach.destinations.iter().map(|&h| (h, f(h))).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (h, _) in scored.into_iter().take(n) {
        out.insert(h);
    }
}

/// Select tiles using multiple strategies to ensure the candidate pool covers
/// offensive, defensive, focus, AoE and kiting positions — not just globally
/// "best" tiles from position_eval.
fn select_diverse_tiles(
    actor_pos: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
    enemies: &[&UnitSnapshot],
) -> Vec<Hex> {
    let mut tiles: HashSet<Hex> = HashSet::new();

    // 1. Offensive: tiles near wounded / high-threat enemies.
    pick_top(reach, 3, &mut tiles, |h| maps.opportunity.get(h));

    // 2. Safe: lowest danger, near healers.
    pick_top(reach, 3, &mut tiles, |h| maps.escape.get(h));

    // 3. Near priority target: closest tiles to the highest-priority enemy.
    if let Some(priority) = enemies.iter().max_by(|a, b| {
        target_priority(active, a, snap)
            .partial_cmp(&target_priority(active, b, snap))
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        pick_top(reach, 2, &mut tiles, |h| {
            -(h.unsigned_distance_to(priority.pos) as f32)
        });
    }

    // 4. AoE origin: tiles from which AoE hits the most enemies.
    if active.tags.contains(AiTags::HAS_AOE) {
        let aoe_radii: Vec<u32> = ctx.abilities.0.iter()
            .filter_map(|id| ctx.db.abilities.get(id))
            .filter_map(|def| match def.aoe {
                AoEShape::Circle { radius } => Some(radius),
                _ => None,
            })
            .collect();

        if !aoe_radii.is_empty() {
            let enemy_positions: HashSet<Hex> = enemies.iter().map(|e| e.pos).collect();
            pick_top(reach, 2, &mut tiles, |h| {
                aoe_radii.iter().map(|&r| {
                    hex_circle(h, r).iter()
                        .filter(|c| enemy_positions.contains(c))
                        .count() as f32
                }).fold(0.0f32, f32::max)
            });
        }
    }

    // 5. Retreat-with-LOS: safe tiles that maintain line of sight to an enemy (kiting).
    if active.tags.contains(AiTags::RANGED) {
        let occupied: HashSet<Hex> = snap.units.iter().map(|u| u.pos).collect();
        let enemy_positions: Vec<Hex> = enemies.iter().map(|e| e.pos).collect();
        pick_top(reach, 2, &mut tiles, |h| {
            let can_see = enemy_positions.iter().any(|&ep| {
                has_los(h, ep, |mid| occupied.contains(&mid) && mid != h && mid != ep)
            });
            if can_see { maps.escape.get(h) } else { f32::NEG_INFINITY }
        });
    }

    // 6. Support coverage: tiles within heal range of wounded allies, ranked
    // by escape. Without this strategy, "retreat + heal wounded ally" combos
    // only surfaced when the destination tile happened to top the generic
    // escape list. Explicit pass guarantees such tiles enter the candidate
    // pool even when competing escape tiles score higher overall.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let heal_range: u32 = ctx.abilities.0.iter()
            .filter_map(|id| ctx.db.abilities.get(id))
            .filter(|def| matches!(def.target_type, TargetType::SingleAlly))
            .map(|def| def.range.max)
            .max()
            .unwrap_or(0);
        if heal_range > 0 {
            let wounded: Vec<Hex> = snap
                .allies_of(active.team)
                .filter(|u| u.entity != active.entity)
                .filter(|u| u.hp < u.max_hp)
                .map(|u| u.pos)
                .collect();
            for ally_pos in &wounded {
                pick_top(reach, 2, &mut tiles, |h| {
                    if h.unsigned_distance_to(*ally_pos) <= heal_range {
                        maps.escape.get(h)
                    } else {
                        f32::NEG_INFINITY
                    }
                });
            }
        }
    }

    // 7. Always include current position (stay-and-cast).
    tiles.insert(actor_pos);

    // Deterministic order: HashSet iteration is random, which makes candidate
    // truncation non-deterministic when many candidates share the same path
    // length. Sort by (x, y) so runs with the same state produce the same pool.
    let mut sorted: Vec<Hex> = tiles.into_iter().collect();
    sorted.sort_by_key(|h| (h.x, h.y));
    sorted
}

// ── Utility scoring ─────────────────────────────────────────────────────────

fn score_candidates(
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
        .map(|factors| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    factors[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }

            // Add noise.
            if noise_amp > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
                score += noise;
            }

            score
        })
        .collect()
}

/// Per-candidate offensive factors (populated only for Cast).
#[derive(Default)]
struct OffensiveFactors {
    damage: f32,
    heal: f32,
    kill: f32,
    cc: f32,
}

/// Compute the 9 raw utility factors for a single candidate.
/// Axes: [damage, kill, cc, heal, position, risk, focus, intent, scarcity].
fn compute_factors(
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
            compute_offensive(ability, *target_pos, *target, candidate.tile, active, ctx, snap)
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
    let intent_val = intent_score(intent, candidate, active, snap, maps, ctx.db, ctx.difficulty);

    apply_reservation_adjustments(candidate, &mut off, &mut focus, &mut position, snap, ctx, reservations);

    let scarcity = match &candidate.kind {
        CandidateKind::Cast { .. } => compute_scarcity(candidate, active, off.kill, ctx, snap),
        CandidateKind::MoveOnly => 0.0,
    };

    [off.damage, off.kill, off.cc, off.heal, position, risk, focus, intent_val, scarcity]
}

/// Compute damage/heal/kill/cc for a Cast candidate.
fn compute_offensive(
    ability: &AbilityId,
    target_pos: Hex,
    target_ent: Entity,
    caster_tile: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> OffensiveFactors {
    let Some(def) = ctx.db.abilities.get(ability) else {
        return OffensiveFactors::default();
    };

    let (mut damage, mut heal) = (0.0f32, 0.0f32);
    match def.aoe {
        AoEShape::None => {
            if let Some(target_unit) = snap.unit(target_ent) {
                let ti = target_info_from_snap(target_unit);
                let raw = score_action(def, &ti, ctx.caster, ctx.db);
                let adjusted = crit_fail_adjusted(raw, def, &ctx.crit_fail_effect, ctx.crit_fail_chance);
                if def.target_type == TargetType::SingleAlly {
                    heal = adjusted;
                } else {
                    damage = adjusted;
                }
            }
        }
        _ => {
            damage = compute_aoe_damage(def, target_pos, caster_tile, active, ctx, snap);
        }
    }

    let kill = compute_kill_flag(def, target_ent, ctx, snap);
    let cc = compute_cc_value(def, target_ent, ctx, snap);

    OffensiveFactors { damage, heal, kill, cc }
}

fn compute_aoe_damage(
    def: &crate::content::abilities::AbilityDef,
    target_pos: Hex,
    caster_tile: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let area: HashSet<Hex> = match def.aoe {
        AoEShape::Circle { radius } => hex_circle(target_pos, radius).into_iter().collect(),
        AoEShape::Line { length } => hex_line(caster_tile, target_pos, length).into_iter().collect(),
        AoEShape::None => HashSet::new(),
    };
    let mut damage = 0.0f32;
    for enemy in snap.enemies_of(active.team) {
        if area.contains(&enemy.pos) {
            let ti = target_info_from_snap(enemy);
            damage += score_action(def, &ti, ctx.caster, ctx.db);
        }
    }
    if def.friendly_fire {
        for ally in snap.allies_of(active.team) {
            if area.contains(&ally.pos) {
                let ti = target_info_from_snap(ally);
                let raw = score_action(def, &ti, ctx.caster, ctx.db).abs();
                let hp_fraction = raw / ally.max_hp.max(1) as f32;
                damage -= raw * (1.0 + hp_fraction);
            }
        }
        if area.contains(&active.pos) {
            let ti = target_info_from_snap(active);
            let raw = score_action(def, &ti, ctx.caster, ctx.db).abs();
            let hp_fraction = raw / active.max_hp.max(1) as f32;
            damage -= raw * (1.0 + hp_fraction);
        }
    }
    crit_fail_adjusted(damage, def, &ctx.crit_fail_effect, ctx.crit_fail_chance)
}

fn compute_kill_flag(
    def: &crate::content::abilities::AbilityDef,
    target_ent: Entity,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    // Kill applies only to damaging abilities aimed at enemies. Without this
    // gate, a heal whose `expected` ≥ target HP would spuriously set kill=1,
    // polluting both the kill factor and scarcity's kill bonus.
    if !matches!(def.target_type, TargetType::SingleEnemy) {
        return 0.0;
    }
    let Some(target_unit) = snap.unit(target_ent) else { return 0.0 };
    let Some(calc) = def.effect.calc(ctx.caster) else { return 0.0 };
    let armor = if calc.pierces_armor { 0.0 } else { (target_unit.armor + target_unit.armor_bonus) as f32 };
    let net = calc.expected() - armor + target_unit.damage_taken_bonus as f32;
    if net >= target_unit.hp as f32 { 1.0 } else { 0.0 }
}

fn compute_cc_value(
    def: &crate::content::abilities::AbilityDef,
    target_ent: Entity,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    def.statuses
        .iter()
        .map(|sa| {
            let Some(sd) = ctx.db.statuses.get(&sa.status) else { return 0.0 };
            let d = sa.duration_rounds as f32;
            let mut val = 0.0f32;
            if sd.skips_turn {
                let target_threat = snap.unit(target_ent).map(|u| u.threat).unwrap_or(1.0);
                val += target_threat * d;
            }
            if sd.damage_taken_bonus > 0 { val += sd.damage_taken_bonus as f32 * d; }
            if sd.armor_bonus > 0 { val += sd.armor_bonus as f32 * d; }
            val
        })
        .sum()
}

/// coordination knob: overkill penalty + focus-fire bonus + duplicate-CC + tile collision.
fn apply_reservation_adjustments(
    candidate: &ActionCandidate,
    off: &mut OffensiveFactors,
    focus: &mut f32,
    position: &mut f32,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
    reservations: &Reservations,
) {
    if let Some(target_ent) = candidate.target() {
        let reserved_dmg = reservations.reserved_damage(target_ent);
        if reserved_dmg > 0.0 {
            if let Some(target_unit) = snap.unit(target_ent) {
                let hp_left = target_unit.hp as f32 - reserved_dmg;
                if hp_left <= 0.0 {
                    off.damage *= ctx.difficulty.overkill_damage_multiplier();
                    off.kill = 0.0;
                } else {
                    *focus *= 1.0 + ctx.difficulty.focus_fire_bonus();
                }
            }
        }
        if reservations.reserved_cc(target_ent) > 0 {
            off.cc *= 0.15;
        }
    }
    if reservations.is_tile_reserved(candidate.tile) {
        *position *= 0.5;
    }
}

// ── Scarcity ────────────────────────────────────────────────────────────────

/// Compute resource-scarcity factor: `swing_value - resource_ratio`.
/// Free abilities return 0.0 (neutral). Expensive abilities on low-value
/// situations get negative scores; expensive abilities in high-swing moments
/// get positive scores.
fn compute_scarcity(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    kill: f32,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let CandidateKind::Cast { ability, target_pos, target } = &candidate.kind else {
        return 0.0;
    };
    let Some(def) = ctx.db.abilities.get(ability) else {
        return 0.0;
    };

    // Free abilities are always neutral.
    if def.costs.is_empty() {
        return 0.0;
    }

    // resource_ratio: max(cost / current_pool) across all resource costs.
    let resource_ratio = def
        .costs
        .iter()
        .map(|c| {
            let pool = match c.resource {
                ResourceKind::Hp => active.hp,
                ResourceKind::Mana => active.mana.map(|(cur, _)| cur).unwrap_or(0),
                ResourceKind::Rage => active.rage.map(|(cur, _)| cur).unwrap_or(0),
                ResourceKind::Energy => active.energy.map(|(cur, _)| cur).unwrap_or(0),
            };
            if pool <= 0 {
                return 1.0;
            }
            (c.amount as f32 / pool as f32).min(1.0)
        })
        .fold(0.0f32, f32::max);

    // swing_value: situational justification for spending.
    let mut swing = 0.0f32;

    // Kill bonus.
    if kill > 0.0 {
        swing += 0.8;
        // Extra value for killing high-value targets.
        if let Some(t) = snap.unit(*target) {
            // Role-based kill bonus scales with target's priority value
            // (Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3).
            // Old behavior: Support/Mage +0.3, Assassin +0.2, others 0.
            // New: smooth gradient via role_value, coeff 0.35 preserves scale.
            swing += 0.35 * t.role.role_value();
        }
    }

    // AoE multi-hit bonus.
    if def.aoe != AoEShape::None {
        let area: Vec<Hex> = match def.aoe {
            AoEShape::Circle { radius } => hex_circle(*target_pos, radius),
            AoEShape::Line { length } => hex_line(candidate.tile, *target_pos, length),
            AoEShape::None => vec![],
        };
        let area_set: HashSet<Hex> = area.into_iter().collect();
        let hits = snap
            .enemies_of(active.team)
            .filter(|e| area_set.contains(&e.pos))
            .count();
        if hits > 1 {
            swing += 0.2 * (hits - 1) as f32;
        }
    }

    // CC on high-threat unstunned target.
    let is_cc = def.statuses.iter().any(|sa| {
        ctx.db
            .statuses
            .get(&sa.status)
            .is_some_and(|sd| sd.skips_turn)
    });
    if is_cc {
        if let Some(t) = snap.unit(*target) {
            if !t.tags.contains(AiTags::IS_STUNNED) {
                swing += 0.5 * (t.threat / 10.0).min(1.0);
            }
        }
    }

    // Overkill penalty: target nearly dead and caster has free attacks.
    if let Some(t) = snap.unit(*target) {
        let target_hp_pct = t.hp as f32 / t.max_hp.max(1) as f32;
        if target_hp_pct < 0.25 && has_free_attack(ctx) {
            swing -= 0.3;
        }
    }

    // Early round penalty: conserve resources at fight start.
    if snap.round <= 1 {
        swing -= 0.15;
    }

    (swing - resource_ratio).clamp(-1.0, 1.0)
}

/// Returns true if the caster has at least one ability with no resource cost.
fn has_free_attack(ctx: &UtilityContext) -> bool {
    ctx.abilities.0.iter().any(|id| {
        ctx.db
            .abilities
            .get(id)
            .is_some_and(|d| d.costs.is_empty() && d.target_type == TargetType::SingleEnemy)
    })
}

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
        let hp_fraction = active.hp as f32 / active.max_hp.max(1) as f32;
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
                    let area: HashSet<Hex> = match def.aoe {
                        AoEShape::Circle { radius } => hex_circle(*target_pos, radius).into_iter().collect(),
                        AoEShape::Line { length } => hex_line(c.tile, *target_pos, length).into_iter().collect(),
                        AoEShape::None => HashSet::new(),
                    };
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
    // Cast: record expected damage + CC on target.
    if let CandidateKind::Cast { ability, target, .. } = &best.kind {
        if let Some(def) = ctx.db.abilities.get(ability) {
            if let Some(target_unit) = snap.unit(*target) {
                let ti = target_info_from_snap(target_unit);
                let dmg = score_action(def, &ti, ctx.caster, ctx.db);
                if dmg > 0.0 {
                    reservations.reserve_damage(*target, dmg);
                }
                let applies_cc = def.statuses.iter().any(|sa| {
                    ctx.db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn)
                });
                if applies_cc {
                    reservations.reserve_cc(*target);
                }
            }
        }
    }

    // Record destination tile (applies to both Cast and MoveOnly).
    if best.tile != actor_pos {
        reservations.reserve_tile(best.tile, active.entity);
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


// ── Helpers ─────────────────────────────────────────────────────────────────

fn target_info_from_snap(u: &UnitSnapshot) -> TargetInfo {
    TargetInfo {
        entity: u.entity,
        pos: u.pos,
        hp: u.hp,
        max_hp: u.max_hp,
        armor: u.armor,
        armor_bonus: u.armor_bonus,
        damage_taken_bonus: u.damage_taken_bonus,
        threat: u.threat,
    }
}

fn can_afford_snap(
    def: &crate::content::abilities::AbilityDef,
    unit: &UnitSnapshot,
) -> bool {
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => unit.hp,
            ResourceKind::Mana => unit.mana.map(|(cur, _)| cur).unwrap_or(0),
            ResourceKind::Rage => unit.rage.map(|(cur, _)| cur).unwrap_or(0),
            ResourceKind::Energy => unit.energy.map(|(cur, _)| cur).unwrap_or(0),
        };
        if available < cost.amount {
            return false;
        }
    }
    true
}

// ── Debug snapshot builder ──────────────────────────────────────────────────

fn format_intent(intent: &TacticalIntent, names: &HashMap<Entity, String>) -> String {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            format!("FocusTarget → {}", names.get(target).map_or("?", |n| n))
        }
        TacticalIntent::ApplyCC { target } => {
            format!("ApplyCC → {}", names.get(target).map_or("?", |n| n))
        }
        TacticalIntent::ProtectAlly { ally } => {
            format!("ProtectAlly → {}", names.get(ally).map_or("?", |n| n))
        }
        TacticalIntent::Reposition => "Reposition".into(),
        TacticalIntent::ProtectSelf => "ProtectSelf".into(),
        TacticalIntent::SetupAOE => "SetupAOE".into(),
        TacticalIntent::LastStand => "LastStand".into(),
    }
}

/// Explain why this intent was selected (re-check conditions from select_intent).
fn explain_intent(
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> String {
    let hp_pct = active.hp as f32 / active.max_hp.max(1) as f32;
    let danger = maps.danger.get(active.pos);

    match intent {
        TacticalIntent::ProtectSelf => {
            format!("hp%={:.0}%<40% AND danger={:.1}", hp_pct * 100.0, danger)
        }
        TacticalIntent::ProtectAlly { ally } => {
            if let Some(a) = snap.unit(*ally) {
                let a_pct = a.hp as f32 / a.max_hp.max(1) as f32;
                format!("CAN_HEAL + ally hp%={:.0}%<50%", a_pct * 100.0)
            } else {
                "CAN_HEAL + wounded ally".into()
            }
        }
        TacticalIntent::FocusTarget { target } => {
            if let Some(t) = snap.unit(*target) {
                let killable = active.threat >= t.hp as f32;
                if killable {
                    format!(
                        "killable: threat={:.1} >= hp={}",
                        active.threat, t.hp,
                    )
                } else {
                    "default: highest target_priority".into()
                }
            } else {
                "default fallback".into()
            }
        }
        TacticalIntent::ApplyCC { .. } => {
            "CAN_CC + unstunned enemy".into()
        }
        TacticalIntent::SetupAOE => {
            "HAS_AOE + enemies clustered (dist≤2)".into()
        }
        TacticalIntent::Reposition => {
            let pos_eval = evaluate_position(active.pos, &active.role, maps);
            format!("position_eval={:.2} < -1.0", pos_eval)
        }
        TacticalIntent::LastStand => {
            format!(
                "hp%={:.0}%, no viable survival option — maximize last action",
                hp_pct * 100.0,
            )
        }
    }
}

fn tile_influence(hex: Hex, role: &AxisProfile, maps: &InfluenceMaps) -> TileInfluence {
    TileInfluence {
        danger: maps.danger.get(hex),
        ally_support: maps.ally_support.get(hex),
        opportunity: maps.opportunity.get(hex),
        escape: maps.escape.get(hex),
        position_eval: evaluate_position(hex, role, maps),
    }
}

fn name_of(entity: Entity, names: &HashMap<Entity, String>) -> String {
    names.get(&entity).cloned().unwrap_or_else(|| format!("{:?}", entity))
}

fn build_fallback_debug(
    active: &UnitSnapshot,
    actor_pos: Hex,
    intent: &TacticalIntent,
    decision: &AiDecision,
    reason: &str,
    _ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    names: &HashMap<Entity, String>,
) -> AiDebugSnapshot {
    // No pick-phase info for fallback paths.
    let actor_name = name_of(active.entity, names);
    let intent_str = format_intent(intent, names);
    let intent_rule = explain_intent(active, intent, snap, maps);

    let priority_target = snap
        .enemies_of(active.team)
        .max_by(|a, b| {
            target_priority(active, a, snap)
                .partial_cmp(&target_priority(active, b, snap))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| (name_of(t.entity, names), target_priority(active, t, snap)));

    let decision_debug = match decision {
        AiDecision::MoveCloser { path } | AiDecision::MoveOnlyRetreat { path } => {
            let label = if matches!(decision, AiDecision::MoveOnlyRetreat { .. }) {
                "MoveOnlyRetreat"
            } else {
                "MoveCloser"
            };
            let dest = path.last().copied().unwrap_or(actor_pos);
            DecisionDebug {
                description: format!(
                    "{} (fallback: {}): {}→{} ({} steps)",
                    label, reason, fmt_offset(actor_pos), fmt_offset(dest), path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence(dest, &active.role, maps)),
            }
        }
        AiDecision::EndTurn => DecisionDebug {
            description: format!("EndTurn (fallback: {})", reason),
            dest_tile: None,
            dest_influence: None,
        },
        _ => DecisionDebug {
            description: format!("fallback: {}", reason),
            dest_tile: None,
            dest_influence: None,
        },
    };

    AiDebugSnapshot {
        actor_name,
        actor: ActorDebug {
            role_label: active.role.dominant_label(),
            pos: hex_to_offset(active.pos),
            hp: active.hp,
            max_hp: active.max_hp,
            threat: active.threat,
            tags: active.tags,
            action: active.action,
            movement: active.movement,
        },
        intent: IntentReasoning {
            intent: intent_str,
            rule: intent_rule,
        },
        priority_target,
        top_candidates: vec![],
        pick: None,
        decision: decision_debug,
        candidate_count: 0,
    }
}

fn build_debug_snapshot(
    active: &UnitSnapshot,
    actor_pos: Hex,
    intent: &TacticalIntent,
    candidates: &[ActionCandidate],
    scores: &[f32],
    decision: &AiDecision,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    names: &HashMap<Entity, String>,
    pick_mech: Option<&PickMechanics>,
    intent_fallback: Option<&(String, f32, f32)>,
) -> AiDebugSnapshot {
    let actor_name = name_of(active.entity, names);
    let intent_str = format_intent(intent, names);
    let intent_rule = match intent_fallback {
        Some((orig, max_align, thresh)) => format!(
            "fallback from {}: max_align={:.2} < threshold={:.2}",
            orig, max_align, thresh,
        ),
        None => explain_intent(active, intent, snap, maps),
    };

    // Priority target.
    let priority_target = snap
        .enemies_of(active.team)
        .max_by(|a, b| {
            target_priority(active, a, snap)
                .partial_cmp(&target_priority(active, b, snap))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| (name_of(t.entity, names), target_priority(active, t, snap)));

    // Top 5 candidates by score — skip -inf masked entries so the log shows
    // only candidates actually in play (ProtectSelf masks non-defensive to -inf).
    let mut indexed: Vec<(usize, f32)> = scores.iter()
        .copied()
        .enumerate()
        .filter(|(_, s)| s.is_finite())
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(5);

    let candidate_count = candidates.len();
    let top_candidates: Vec<CandidateDebug> = indexed
        .iter()
        .map(|&(i, total)| {
            let c = &candidates[i];
            let raw = compute_factors(c, active, intent, ctx, snap, maps, reservations);
            let (ability_label, target_name, is_move_only) = match &c.kind {
                CandidateKind::Cast { ability, target, .. } => {
                    (ability.0.clone(), name_of(*target, names), false)
                }
                CandidateKind::MoveOnly => (String::new(), String::new(), true),
            };
            CandidateDebug {
                ability: ability_label,
                target_name,
                tile: hex_to_offset(c.tile),
                tile_influence: tile_influence(c.tile, &active.role, maps),
                raw,
                total,
                is_move_only,
            }
        })
        .collect();

    // Decision description.
    let decision_debug = match decision {
        AiDecision::CastInPlace { ability, target, .. } => DecisionDebug {
            description: format!(
                "CastInPlace: {} → {} (stay at {})",
                ability, name_of(*target, names), fmt_offset(actor_pos),
            ),
            dest_tile: None,
            dest_influence: None,
        },
        AiDecision::MoveAndCast { path, ability, target, .. } => {
            let dest = path.last().copied().unwrap_or(actor_pos);
            DecisionDebug {
                description: format!(
                    "MoveAndCast: {} → {} → {} ({}→{}, {} steps)",
                    fmt_offset(actor_pos), fmt_offset(dest),
                    format_args!("{} → {}", ability, name_of(*target, names)),
                    fmt_offset(actor_pos), fmt_offset(dest), path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence(dest, &active.role, maps)),
            }
        }
        AiDecision::MoveCloser { path } | AiDecision::MoveOnlyRetreat { path } => {
            let label = if matches!(decision, AiDecision::MoveOnlyRetreat { .. }) {
                "MoveOnlyRetreat"
            } else {
                "MoveCloser"
            };
            let dest = path.last().copied().unwrap_or(actor_pos);
            DecisionDebug {
                description: format!(
                    "{}: {}→{} ({} steps)",
                    label, fmt_offset(actor_pos), fmt_offset(dest), path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence(dest, &active.role, maps)),
            }
        }
        AiDecision::EndTurn => DecisionDebug {
            description: "EndTurn (no action/movement)".into(),
            dest_tile: None,
            dest_influence: None,
        },
    };

    let pick = pick_mech.map(|pm| PickDebug {
        top_k: pm.top_k,
        window: pm.window,
        mercy_margin: pm.mercy_margin,
        mercy_applied: pm.mercy_applied,
        pool: pm
            .pool
            .iter()
            .map(|&(idx, score)| {
                let c = &candidates[idx];
                let label = match &c.kind {
                    CandidateKind::Cast { ability, target, .. } => {
                        format!("{} → {}", ability, name_of(*target, names))
                    }
                    CandidateKind::MoveOnly => {
                        format!("retreat to {}", fmt_offset(c.tile))
                    }
                };
                PoolEntry { label, score }
            })
            .collect(),
        chosen_pos: pm.chosen_pos,
    });

    AiDebugSnapshot {
        actor_name,
        actor: ActorDebug {
            role_label: active.role.dominant_label(),
            pos: hex_to_offset(active.pos),
            hp: active.hp,
            max_hp: active.max_hp,
            threat: active.threat,
            tags: active.tags,
            action: active.action,
            movement: active.movement,
        },
        intent: IntentReasoning {
            intent: intent_str,
            rule: intent_rule,
        },
        priority_target,
        top_candidates,
        pick,
        decision: decision_debug,
        candidate_count,
    }
}

fn fmt_offset(hex: Hex) -> String {
    let [q, r] = hex_to_offset(hex);
    format!("({},{})", q, r)
}

fn crit_fail_adjusted(
    score: f32,
    def: &crate::content::abilities::AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
    match effect {
        CritFailEffect::ManaOverload => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score - chance * mana_cost
        }
        CritFailEffect::CircuitBreach => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score * (1.0 - chance) - chance * mana_cost * 0.5
        }
        _ => score * (1.0 - chance),
    }
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

    /// Build a ReachableMap where all in-bounds cells are reachable.
    fn fake_reach(start: Hex) -> ReachableMap {
        use crate::game::pathfinding::reachable_with_paths;
        reachable_with_paths(start, 20, in_bounds, |_| true)
    }

    #[test]
    fn diverse_tiles_always_includes_current_pos() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        let enemy = unit(1, Team::Player, hex_from_offset(0, 0));
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let db = GameDb::default();
        let difficulty = DifficultyProfile::default();
        let ctx = UtilityContext {
            db: &db,
            difficulty: &difficulty,
            caster: &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            abilities: &Abilities(vec!["melee_attack".into()]),
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        };
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();
        let reach = fake_reach(actor_pos);
        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        assert!(tiles.contains(&actor_pos), "current position must always be included");
    }

    #[test]
    fn diverse_tiles_near_priority_target() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        // Wounded high-priority target at (2,3).
        let mut target = unit(1, Team::Player, hex_from_offset(2, 3));
        target.hp = 3;
        target.threat = 10.0;

        let s = snap(vec![active.clone(), target.clone()]);
        let maps = empty_maps();
        let db = GameDb::default();
        let difficulty = DifficultyProfile::default();
        let ctx = UtilityContext {
            db: &db,
            difficulty: &difficulty,
            caster: &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            abilities: &Abilities(vec!["melee_attack".into()]),
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        };
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();

        let target_hex = hex_from_offset(2, 3);
        let reach = fake_reach(actor_pos);

        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        // "Near priority target" strategy should include at least one tile within 1 hex of the target.
        let has_close = tiles.iter().any(|&h| h.unsigned_distance_to(target_hex) <= 1);
        assert!(
            has_close,
            "should include a tile near priority target; got {:?}",
            tiles.iter().map(|h| hex_to_offset(*h)).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn diverse_tiles_includes_offensive_and_safe() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        let enemy = unit(1, Team::Player, hex_from_offset(1, 1));

        let s = snap(vec![active.clone(), enemy]);

        // Set up maps: offensive tile at (3,2), safe tile at (5,4).
        let offensive = hex_from_offset(3, 2);
        let safe = hex_from_offset(5, 4);
        let mut maps = empty_maps();
        maps.opportunity.add(offensive, 0.9);
        maps.escape.add(safe, 0.9);

        let db = GameDb::default();
        let difficulty = DifficultyProfile::default();
        let ctx = UtilityContext {
            db: &db,
            difficulty: &difficulty,
            caster: &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            abilities: &Abilities(vec!["melee_attack".into()]),
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        };
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();
        let reach = fake_reach(actor_pos);

        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        assert!(tiles.contains(&offensive), "offensive tile should be included");
        assert!(tiles.contains(&safe), "safe tile should be included");
    }

    // ── Sanity check tests ──────────────────────────────────────────────

    fn cast(tile: Hex, ability: &str, target_pos: Hex, target: Entity) -> ActionCandidate {
        ActionCandidate {
            tile,
            path: vec![],
            kind: CandidateKind::Cast {
                ability: ability.into(),
                target_pos,
                target,
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

    // ── Scarcity tests ─────────────────────────────────────────────────

    fn scarcity_ctx<'a>(db: &'a GameDb, difficulty: &'a DifficultyProfile, abilities: &'a Abilities) -> UtilityContext<'a> {
        UtilityContext {
            db,
            difficulty,
            caster: &CasterContext { str_mod: 0, int_mod: 3, spell_power: 0, weapon_dice: None },
            abilities,
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        }
    }

    #[test]
    fn scarcity_neutral_for_free_abilities() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        let s = snap(vec![active.clone(), enemy.clone()]);
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = scarcity_ctx(&db, &diff, &abilities);

        let c = candidate(tile, enemy.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert_eq!(score, 0.0, "free ability should have zero scarcity");
    }

    #[test]
    fn scarcity_penalizes_expensive_on_dying_target() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let mut enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        enemy.hp = 1; // nearly dead
        enemy.max_hp = 20;

        let s = snap(vec![active.clone(), enemy.clone()]);
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        // Has both fireball (5 mana) and melee_attack (free).
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&db, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert!(
            score < 0.0,
            "expensive ability on dying target should get negative scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_kill_on_support() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let mut enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        enemy.role = AxisProfile::from(AiRole::Support);
        enemy.hp = 5;
        enemy.max_hp = 20;

        let s = snap(vec![active.clone(), enemy.clone()]);
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&db, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);
        // kill=1.0 means this is a confirmed kill.
        let score = compute_scarcity(&c, &active, 1.0, &ctx, &s);
        assert!(
            score > 0.0,
            "kill on support should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_aoe_on_cluster() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((20, 20)); // large pool → low resource_ratio

        // Place 3 enemies adjacent to each other (within AoE circle r=1).
        let center = hex_from_offset(2, 3);
        let neighbors: Vec<Hex> = center.all_neighbors().to_vec();
        let e1 = unit(1, Team::Player, center);
        let e2 = unit(2, Team::Player, neighbors[0]);
        let e3 = unit(3, Team::Player, neighbors[1]);

        let s = BattleSnapshot {
            units: vec![active.clone(), e1.clone(), e2.clone(), e3.clone()],
            active_unit: active.entity,
            round: 3, // past early-round penalty
        };
        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&db, &diff, &abilities);

        // Target pos at e1, fireball has AoE circle radius 1 → hits all 3.
        let c = cast(tile, "fireball", e1.pos, e1.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert!(
            score > 0.0,
            "AoE on cluster should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_penalizes_early_round_spend() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));

        let db = GameDb::default();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into()]);
        let ctx = scarcity_ctx(&db, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);

        // Round 1 — early penalty applies.
        let s_r1 = BattleSnapshot {
            units: vec![active.clone(), enemy.clone()],
            active_unit: active.entity,
            round: 1,
        };
        let score_r1 = compute_scarcity(&c, &active, 0.0, &ctx, &s_r1);

        // Round 3 — no early penalty.
        let s_r3 = BattleSnapshot {
            units: vec![active.clone(), enemy.clone()],
            active_unit: active.entity,
            round: 3,
        };
        let score_r3 = compute_scarcity(&c, &active, 0.0, &ctx, &s_r3);

        assert!(
            score_r1 < score_r3,
            "round 1 ({:.2}) should have lower scarcity than round 3 ({:.2})",
            score_r1, score_r3,
        );
    }

    // ── Normalization tests ───────────────────────────────────────────

    #[test]
    fn signed_normalization_preserves_negative_order() {
        // Simulate signed factor values: all negative.
        // Symmetric normalization should preserve order, not collapse to 0.
        let values = [-3.0f32, -1.0, -0.5];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let normalized: Vec<f32> = values.iter().map(|v| v / max_abs).collect();
        assert_eq!(normalized, vec![-1.0, -1.0 / 3.0, -0.5 / 3.0]);
        // Order preserved: most negative stays most negative.
        assert!(normalized[0] < normalized[1]);
        assert!(normalized[1] < normalized[2]);
    }

    #[test]
    fn signed_normalization_flat_batch_gives_zero() {
        // All candidates have the same signed factor value → denom = |v|, norm = ±1.
        // If all zero → denom = 0 → normalized = 0 (not NaN/inf).
        let values = [0.0f32; 3];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        for &v in &values {
            let norm = if max_abs > f32::EPSILON { v / max_abs } else { 0.0 };
            assert_eq!(norm, 0.0);
            assert!(!norm.is_nan());
        }
    }
}
