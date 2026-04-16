use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::ActionCandidate;
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::{hex_circle, hex_line, Hex};
use crate::game::resources::GameDb;
use bevy::prelude::Entity;
use std::collections::HashSet;

// ── Intent enum ─────────────────────────────────────────────────────────────

pub enum TacticalIntent {
    /// Focus fire: kill or heavily damage a specific target.
    FocusTarget { target: Entity },
    /// Apply CC (stun) to a high-threat target.
    ApplyCC { target: Entity },
    /// Reposition to a better tile.
    Reposition,
    /// Self-preservation: avoid danger.
    ProtectSelf,
    /// Protect/heal a specific wounded ally.
    ProtectAlly { ally: Entity },
    /// Position to hit multiple enemies with AoE.
    SetupAOE,
}

// ── Intent selection ────────────────────────────────────────────────────────

/// Analyze the battlefield and pick one high-level tactical goal.
/// Priority-based: first matching condition wins.
pub fn select_intent(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    difficulty: &DifficultyProfile,
) -> TacticalIntent {
    // 1. ProtectSelf: critically wounded and in danger.
    let hp_pct = active.hp as f32 / active.max_hp.max(1) as f32;
    if hp_pct < 0.25 && maps.danger.get(active.pos) > active.hp as f32 {
        return TacticalIntent::ProtectSelf;
    }

    // 2. ProtectAlly: wounded ally below 30% and we can heal.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let most_wounded = snap
            .allies_of(active.team)
            .filter(|u| u.entity != active.entity)
            .filter(|u| (u.hp as f32 / u.max_hp.max(1) as f32) < 0.3)
            .min_by_key(|u| u.hp);
        if let Some(ally) = most_wounded {
            return TacticalIntent::ProtectAlly { ally: ally.entity };
        }
    }

    // 3. FocusTarget: killable enemy (awareness scales recognition).
    let killable = snap
        .enemies_of(active.team)
        .filter(|e| active.threat * difficulty.awareness >= e.hp as f32)
        .min_by_key(|e| e.hp);
    if let Some(target) = killable {
        return TacticalIntent::FocusTarget { target: target.entity };
    }

    // 4. ApplyCC: high-threat unstunned enemy.
    if active.tags.contains(AiTags::CAN_CC) {
        let cc_target = snap
            .enemies_of(active.team)
            .filter(|e| !e.tags.contains(AiTags::IS_STUNNED))
            .max_by(|a, b| a.threat.partial_cmp(&b.threat).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(target) = cc_target {
            return TacticalIntent::ApplyCC { target: target.entity };
        }
    }

    // 5. SetupAOE: has AoE and enemies are clustered (≥1 pair within 2 hexes).
    if active.tags.contains(AiTags::HAS_AOE) {
        let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
        let clustered = enemies.iter().enumerate().any(|(i, a)| {
            enemies[i + 1..]
                .iter()
                .any(|b| a.pos.unsigned_distance_to(b.pos) <= 2)
        });
        if clustered {
            return TacticalIntent::SetupAOE;
        }
    }

    // 6. Reposition: current position is actively bad.
    if evaluate_position(active.pos, active.role, maps) < -1.0 {
        return TacticalIntent::Reposition;
    }

    // 7. Default: focus highest-priority target.
    let default_target = snap
        .enemies_of(active.team)
        .max_by(|a, b| {
            target_priority(active, a, snap)
                .partial_cmp(&target_priority(active, b, snap))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| t.entity);

    match default_target {
        Some(target) => TacticalIntent::FocusTarget { target },
        None => TacticalIntent::Reposition,
    }
}

// ── Intent → constraint filter ──────────────────────────────────────────────

/// Apply intent-specific filters to candidates.
/// If filtering would remove ALL candidates, the filter is skipped (fallback).
pub fn apply_intent_filter(
    candidates: &mut Vec<ActionCandidate>,
    intent: &TacticalIntent,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    db: &GameDb,
) {
    let keep: Vec<bool> = candidates
        .iter()
        .map(|c| intent_keeps(c, intent, active, snap, maps, db))
        .collect();

    // Only apply if at least one candidate survives.
    if keep.iter().any(|&k| k) {
        let mut idx = 0;
        candidates.retain(|_| {
            let r = keep[idx];
            idx += 1;
            r
        });
    }
}

fn intent_keeps(
    c: &ActionCandidate,
    intent: &TacticalIntent,
    active: &UnitSnapshot,
    _snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    db: &GameDb,
) -> bool {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            let Some(def) = db.abilities.get(&c.ability) else { return true };
            // Allow heals through.
            if def.target_type == TargetType::SingleAlly {
                return true;
            }
            c.target == *target
        }
        TacticalIntent::ApplyCC { target } => {
            let Some(def) = db.abilities.get(&c.ability) else { return true };
            let is_cc = def.statuses.iter().any(|sa| {
                db.statuses
                    .get(&sa.status)
                    .is_some_and(|sd| sd.skips_turn)
            });
            // CC abilities must target the intent target; others pass.
            if is_cc { c.target == *target } else { true }
        }
        TacticalIntent::ProtectSelf => {
            maps.danger.get(c.tile) <= active.hp as f32 * 0.5
        }
        TacticalIntent::ProtectAlly { ally } => {
            let Some(def) = db.abilities.get(&c.ability) else { return true };
            if def.target_type == TargetType::SingleAlly {
                c.target == *ally
            } else {
                true
            }
        }
        TacticalIntent::SetupAOE => {
            let Some(def) = db.abilities.get(&c.ability) else { return true };
            def.aoe != AoEShape::None
        }
        TacticalIntent::Reposition => true,
    }
}

// ── Intent → utility score (factor[7]) ──────────────────────────────────────

/// Compute how well a candidate aligns with the current intent.
/// Replaces the old role-based `compute_intent`.
pub fn intent_score(
    intent: &TacticalIntent,
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    db: &GameDb,
) -> f32 {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            if candidate.target == *target { 1.0 } else { 0.0 }
        }
        TacticalIntent::ApplyCC { target } => {
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return 0.0;
            };
            let is_cc = def.statuses.iter().any(|sa| {
                db.statuses
                    .get(&sa.status)
                    .is_some_and(|sd| sd.skips_turn)
            });
            if is_cc && candidate.target == *target {
                1.0
            } else if candidate.target == *target {
                0.5
            } else {
                0.0
            }
        }
        TacticalIntent::Reposition => {
            evaluate_position(candidate.tile, active.role, maps).max(0.0)
        }
        TacticalIntent::ProtectSelf => {
            (-maps.danger.get(candidate.tile) + active.armor as f32).max(0.0)
        }
        TacticalIntent::ProtectAlly { ally } => {
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return 0.0;
            };
            if def.target_type == TargetType::SingleAlly && candidate.target == *ally {
                1.0
            } else if snap
                .unit(*ally)
                .is_some_and(|a| candidate.tile.unsigned_distance_to(a.pos) <= 1)
            {
                0.5
            } else {
                0.0
            }
        }
        TacticalIntent::SetupAOE => {
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return 0.0;
            };
            if def.aoe == AoEShape::None {
                return 0.0;
            }
            let area: Vec<Hex> = match def.aoe {
                AoEShape::Circle { radius } => hex_circle(candidate.target_pos, radius),
                AoEShape::Line { length } => {
                    hex_line(candidate.tile, candidate.target_pos, length)
                }
                AoEShape::None => vec![],
            };
            let area_set: HashSet<Hex> = area.into_iter().collect();
            let total = snap.enemies_of(active.team).count() as f32;
            let hit = snap
                .enemies_of(active.team)
                .filter(|e| area_set.contains(&e.pos))
                .count() as f32;
            if total > 0.0 { hit / total } else { 0.0 }
        }
    }
}
