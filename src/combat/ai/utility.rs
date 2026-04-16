#![allow(clippy::too_many_arguments)]
use crate::combat::ai::constraints::filter_candidates;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{apply_intent_filter, intent_score, select_intent, TacticalIntent};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::role::AiRole;
use crate::combat::ai::scoring::{score_action, TargetInfo};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::content::abilities::{AoEShape, CasterContext, TargetType};
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng, ResourceKind};
use crate::game::components::{Abilities, Team};
use crate::game::hex::{hex_circle, hex_line, in_bounds, Hex};
use crate::game::pathfinding::ReachableMap;
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;
use hexx::EdgeDirection;
use crate::combat::ai::debug::{
    ActorDebug, AiDebugSnapshot, CandidateDebug, DecisionDebug, IntentReasoning, TileInfluence,
};
use crate::game::hex::hex_to_offset;
use std::collections::{HashMap, HashSet};

// ── Public types ────────────────────────────────────────────────────────────

pub struct ActionCandidate {
    pub tile: Hex,
    pub path: Vec<Hex>,
    pub ability: AbilityId,
    pub target_pos: Hex,
    pub target: Entity,
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
    EndTurn,
}

// ── Role weight tables ──────────────────────────────────────────────────────

/// 8 utility factors: damage, kill, cc, heal, position, risk, focus, intent.
const NUM_FACTORS: usize = 8;

#[rustfmt::skip]
const ROLE_WEIGHTS: [[f32; NUM_FACTORS]; 5] = [
    //            dmg   kill  cc    heal  pos   risk  focus intent
    /* Bruiser */ [1.0,  1.5,  0.3,  0.0,  0.5,  0.3,  0.8,  1.0],
    /* Archer  */ [1.0,  1.0,  0.3,  0.0,  1.0,  0.8,  0.5,  1.0],
    /* Mage    */ [0.8,  0.8,  1.2,  0.0,  0.8,  0.6,  0.5,  1.0],
    /* Support */ [0.2,  0.3,  0.8,  2.0,  1.0,  1.0,  0.5,  1.0],
    /* Assassin*/ [0.8,  2.0,  0.2,  0.0,  0.3,  0.2,  1.5,  1.0],
];

fn role_index(role: AiRole) -> usize {
    match role {
        AiRole::Bruiser => 0,
        AiRole::Archer => 1,
        AiRole::Mage => 2,
        AiRole::Support => 3,
        AiRole::Assassin => 4,
    }
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
    positions: &HexPositions,
    reach: &ReachableMap,
    rng: &mut DiceRng,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>) {
    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None);
    };

    // ── Select tactical intent ──────────────────────────────────────────
    let intent = select_intent(active, snap, maps, ctx.difficulty);

    // ── Generate candidates ─────────────────────────────────────────────
    let mut candidates = generate_candidates(actor_pos, active, ctx, snap, maps, positions, reach);

    if candidates.is_empty() {
        return (fallback_move(actor_pos, active, ctx, snap, reach), None);
    }

    // ── Hard constraints ────────────────────────────────────────────────
    filter_candidates(&mut candidates, active, snap, maps, ctx.db);

    if candidates.is_empty() {
        return (fallback_move(actor_pos, active, ctx, snap, reach), None);
    }

    // ── Intent filter ───────────────────────────────────────────────────
    apply_intent_filter(&mut candidates, &intent, active, snap, maps, ctx.db);

    // ── Utility scoring ─────────────────────────────────────────────────
    let scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, rng);

    // Pick best.
    let best_idx = scored
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);

    // Build debug snapshot before swap_remove invalidates indices.
    let debug_snapshot = if debug {
        let best = &candidates[best_idx];
        let decision_preview = if best.tile == actor_pos {
            AiDecision::CastInPlace {
                ability: best.ability.clone(),
                target: best.target,
                target_pos: best.target_pos,
            }
        } else {
            AiDecision::MoveAndCast {
                path: best.path.clone(),
                ability: best.ability.clone(),
                target: best.target,
                target_pos: best.target_pos,
            }
        };
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &candidates, &scored, &decision_preview,
            ctx, snap, maps, debug_names,
        ))
    } else {
        None
    };

    let best = candidates.swap_remove(best_idx);

    let decision = if best.tile == actor_pos {
        AiDecision::CastInPlace {
            ability: best.ability,
            target: best.target,
            target_pos: best.target_pos,
        }
    } else {
        AiDecision::MoveAndCast {
            path: best.path,
            ability: best.ability,
            target: best.target,
            target_pos: best.target_pos,
        }
    };

    (decision, debug_snapshot)
}

// ── Candidate generation ────────────────────────────────────────────────────

/// Max number of reachable tiles to evaluate (plus current position).
const MAX_TILES: usize = 8;

fn generate_candidates(
    actor_pos: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    positions: &HexPositions,
    reach: &ReachableMap,
) -> Vec<ActionCandidate> {
    // Score all reachable tiles, take top-N.
    let mut tile_scores: Vec<(Hex, f32)> = reach
        .destinations
        .iter()
        .map(|&h| (h, evaluate_position(h, active.role, maps) * ctx.difficulty.awareness))
        .collect();
    tile_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    tile_scores.truncate(MAX_TILES);

    // Always include current position (stay-and-cast).
    let mut tiles: Vec<Hex> = tile_scores.into_iter().map(|(h, _)| h).collect();
    if !tiles.contains(&actor_pos) {
        tiles.push(actor_pos);
    }

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();

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
                let target_entity = positions.entity_at(target_pos).unwrap_or(active.entity);

                candidates.push(ActionCandidate {
                    tile,
                    path: path.clone(),
                    ability: ability_id.clone(),
                    target_pos,
                    target: target_entity,
                });
            }
        }
    }

    // Deduplicate: same (ability, target) from different tiles — keep shortest path.
    candidates.sort_by(|a, b| a.path.len().cmp(&b.path.len()));
    let mut seen: HashSet<(AbilityId, Entity)> = HashSet::new();
    candidates.retain(|c| seen.insert((c.ability.clone(), c.target)));

    // Cap total candidates.
    candidates.truncate(25);
    candidates
}

// ── Utility scoring ─────────────────────────────────────────────────────────

fn score_candidates(
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    rng: &mut DiceRng,
) -> Vec<f32> {
    if candidates.is_empty() {
        return vec![];
    }

    // Compute raw factors for each candidate.
    let raw: Vec<[f32; NUM_FACTORS]> = candidates
        .iter()
        .map(|c| compute_factors(c, active, intent, ctx, snap, maps))
        .collect();

    // Find max per factor for normalization.
    let mut maxes = [0.0f32; NUM_FACTORS];
    for factors in &raw {
        for (i, &v) in factors.iter().enumerate() {
            if v > maxes[i] {
                maxes[i] = v;
            }
        }
    }

    // Normalize and apply role weights.
    let weights = &ROLE_WEIGHTS[role_index(active.role)];

    raw.iter()
        .map(|factors| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if maxes[i] > 0.0 {
                    factors[i] / maxes[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }

            // Add noise.
            if ctx.difficulty.noise > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * ctx.difficulty.noise;
                score += noise;
            }

            score
        })
        .collect()
}

/// Compute the 8 raw utility factors for a single candidate.
/// [damage, kill, cc, heal, position, risk, focus, intent]
fn compute_factors(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> [f32; NUM_FACTORS] {
    let Some(def) = ctx.db.abilities.get(&candidate.ability) else {
        return [0.0; NUM_FACTORS];
    };

    let target_info = snap.unit(candidate.target).map(target_info_from_snap);

    // ── damage / heal ───────────────────────────────────────────────────
    let (mut damage, mut heal) = (0.0f32, 0.0f32);

    match def.aoe {
        AoEShape::None => {
            if let Some(ref ti) = target_info {
                let raw = score_action(def, ti, ctx.caster, ctx.db, ctx.difficulty);
                let adjusted = crit_fail_adjusted(raw, def, &ctx.crit_fail_effect, ctx.crit_fail_chance);
                if def.target_type == TargetType::SingleAlly {
                    heal = adjusted;
                } else {
                    damage = adjusted;
                }
            }
        }
        _ => {
            // AoE: sum over affected units.
            let area: Vec<Hex> = match def.aoe {
                AoEShape::Circle { radius } => hex_circle(candidate.target_pos, radius),
                AoEShape::Line { length } => hex_line(candidate.tile, candidate.target_pos, length),
                AoEShape::None => vec![],
            };
            let area_set: HashSet<Hex> = area.into_iter().collect();
            for enemy in snap.enemies_of(active.team) {
                if area_set.contains(&enemy.pos) {
                    let ti = target_info_from_snap(enemy);
                    damage += score_action(def, &ti, ctx.caster, ctx.db, ctx.difficulty);
                }
            }
            if def.friendly_fire {
                for ally in snap.allies_of(active.team) {
                    if ally.entity != active.entity && area_set.contains(&ally.pos) {
                        let ti = target_info_from_snap(ally);
                        damage -= score_action(def, &ti, ctx.caster, ctx.db, ctx.difficulty).abs();
                    }
                }
            }
            damage = crit_fail_adjusted(damage, def, &ctx.crit_fail_effect, ctx.crit_fail_chance);
        }
    }

    // ── kill ─────────────────────────────────────────────────────────────
    let kill = if let Some(target_unit) = snap.unit(candidate.target) {
        if let Some(calc) = def.effect.calc(ctx.caster) {
            let expected = calc.expected();
            let armor = if calc.pierces_armor {
                0.0
            } else {
                (target_unit.armor + target_unit.armor_bonus) as f32 * ctx.difficulty.armor_awareness
            };
            let net = expected - armor + target_unit.damage_taken_bonus as f32;
            if net >= target_unit.hp as f32 { 1.0 } else { 0.0 }
        } else {
            0.0
        }
    } else {
        0.0
    };

    // ── cc ───────────────────────────────────────────────────────────────
    let cc = def
        .statuses
        .iter()
        .map(|sa| {
            let Some(sd) = ctx.db.statuses.get(&sa.status) else {
                return 0.0;
            };
            let d = sa.duration_rounds as f32;
            let mut val = 0.0f32;
            if sd.skips_turn {
                let target_threat = snap
                    .unit(candidate.target)
                    .map(|u| u.threat)
                    .unwrap_or(1.0);
                val += target_threat * d;
            }
            if sd.damage_taken_bonus > 0 {
                val += sd.damage_taken_bonus as f32 * d;
            }
            if sd.armor_bonus > 0 {
                val += sd.armor_bonus as f32 * d;
            }
            val
        })
        .sum::<f32>()
        * ctx.difficulty.status_value_scale;

    // ── position ─────────────────────────────────────────────────────────
    let position = evaluate_position(candidate.tile, active.role, maps) * ctx.difficulty.awareness;

    // ── risk ─────────────────────────────────────────────────────────────
    // Lower danger = better. We invert so higher = safer.
    let danger = maps.danger.get(candidate.tile);
    let risk = (-danger + active.armor as f32).max(0.0);

    // ── focus ────────────────────────────────────────────────────────────
    let focus = snap
        .unit(candidate.target)
        .map(|target_unit| target_priority(active, target_unit, snap))
        .unwrap_or(0.0);

    // ── intent ───────────────────────────────────────────────────────────
    let intent_val = intent_score(intent, candidate, active, snap, maps, ctx.db);

    [damage, kill, cc, heal, position, risk, focus, intent_val]
}

// ── Fallback ────────────────────────────────────────────────────────────────

/// When no attack candidates exist, try to move closer to enemies.
fn fallback_move(
    _actor_pos: Hex,
    active: &UnitSnapshot,
    _ctx: &UtilityContext,
    snap: &BattleSnapshot,
    reach: &ReachableMap,
) -> AiDecision {
    if !active.movement {
        return AiDecision::EndTurn;
    }

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    if enemies.is_empty() {
        return AiDecision::EndTurn;
    }

    // Find reachable tile closest to any enemy.
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
    }
}

/// Explain why this intent was selected (re-check conditions from select_intent).
fn explain_intent(
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    difficulty: &DifficultyProfile,
) -> String {
    let hp_pct = active.hp as f32 / active.max_hp.max(1) as f32;
    let danger = maps.danger.get(active.pos);

    match intent {
        TacticalIntent::ProtectSelf => {
            format!("hp%={:.0}%<25% AND danger={:.1}>hp={}", hp_pct * 100.0, danger, active.hp)
        }
        TacticalIntent::ProtectAlly { ally } => {
            if let Some(a) = snap.unit(*ally) {
                let a_pct = a.hp as f32 / a.max_hp.max(1) as f32;
                format!("CAN_HEAL + ally hp%={:.0}%<30%", a_pct * 100.0)
            } else {
                "CAN_HEAL + wounded ally".into()
            }
        }
        TacticalIntent::FocusTarget { target } => {
            if let Some(t) = snap.unit(*target) {
                let killable = active.threat * difficulty.awareness >= t.hp as f32;
                if killable {
                    format!(
                        "killable: threat={:.1}×awareness={:.1}={:.1} >= hp={}",
                        active.threat, difficulty.awareness,
                        active.threat * difficulty.awareness, t.hp,
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
            let pos_eval = evaluate_position(active.pos, active.role, maps);
            format!("position_eval={:.2} < -1.0", pos_eval)
        }
    }
}

fn tile_influence(hex: Hex, role: AiRole, maps: &InfluenceMaps) -> TileInfluence {
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
    names: &HashMap<Entity, String>,
) -> AiDebugSnapshot {
    let actor_name = name_of(active.entity, names);
    let intent_str = format_intent(intent, names);
    let intent_rule = explain_intent(active, intent, snap, maps, ctx.difficulty);

    // Priority target.
    let priority_target = snap
        .enemies_of(active.team)
        .max_by(|a, b| {
            target_priority(active, a, snap)
                .partial_cmp(&target_priority(active, b, snap))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| (name_of(t.entity, names), target_priority(active, t, snap)));

    // Top 5 candidates by score.
    let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(5);

    let candidate_count = candidates.len();
    let top_candidates: Vec<CandidateDebug> = indexed
        .iter()
        .map(|&(i, total)| {
            let c = &candidates[i];
            let raw = compute_factors(c, active, intent, ctx, snap, maps);
            CandidateDebug {
                ability: c.ability.0.clone(),
                target_name: name_of(c.target, names),
                tile: hex_to_offset(c.tile),
                tile_influence: tile_influence(c.tile, active.role, maps),
                raw,
                total,
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
                dest_influence: Some(tile_influence(dest, active.role, maps)),
            }
        }
        AiDecision::MoveCloser { path } => {
            let dest = path.last().copied().unwrap_or(actor_pos);
            DecisionDebug {
                description: format!(
                    "MoveCloser: {}→{} ({} steps, no attack available)",
                    fmt_offset(actor_pos), fmt_offset(dest), path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence(dest, active.role, maps)),
            }
        }
        AiDecision::EndTurn => DecisionDebug {
            description: "EndTurn (no action/movement)".into(),
            dest_tile: None,
            dest_influence: None,
        },
    };

    AiDebugSnapshot {
        actor_name,
        actor: ActorDebug {
            role: active.role,
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
