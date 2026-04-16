use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::ActionCandidate;
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::{hex_circle, hex_line, Hex};
use crate::game::resources::GameDb;
use bevy::prelude::*;
use std::collections::HashSet;

/// Penalty values for soft intent misalignment.
const MISALIGN_PENALTY: f32 = -0.5;
const MILD_PENALTY: f32 = -0.3;

/// Bonus multiplier for continuing the same intent (stickiness).
const STICKINESS_BONUS: f32 = 0.25;
/// Same target bonus on top of stickiness.
const TARGET_STICKINESS_BONUS: f32 = 0.15;
/// Max turns an intent can receive stickiness bonus.
const MAX_COMMITTED_TURNS: u8 = 3;

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
    /// Survival is unlikely — maximize last useful action (kill > cc > damage).
    LastStand,
}

/// Intent kind without target data, for stickiness comparison.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    FocusTarget,
    ApplyCC,
    Reposition,
    ProtectSelf,
    ProtectAlly,
    SetupAOE,
    LastStand,
}

impl TacticalIntent {
    pub fn kind(&self) -> IntentKind {
        match self {
            Self::FocusTarget { .. } => IntentKind::FocusTarget,
            Self::ApplyCC { .. } => IntentKind::ApplyCC,
            Self::Reposition => IntentKind::Reposition,
            Self::ProtectSelf => IntentKind::ProtectSelf,
            Self::ProtectAlly { .. } => IntentKind::ProtectAlly,
            Self::SetupAOE => IntentKind::SetupAOE,
            Self::LastStand => IntentKind::LastStand,
        }
    }

    pub fn target(&self) -> Option<Entity> {
        match self {
            Self::FocusTarget { target } | Self::ApplyCC { target } => Some(*target),
            Self::ProtectAlly { ally } => Some(*ally),
            _ => None,
        }
    }
}

// ── Persistent AI memory ───────────────────────────────────────────────────

#[derive(Component, Default)]
pub struct AiMemory {
    pub last_intent: Option<IntentKind>,
    pub last_target: Option<Entity>,
    pub turns_committed: u8,
}

// ── Intent selection (scored + hysteresis) ──────────────────────────────────

/// Analyze the battlefield, score all valid intents, and pick the best.
/// Applies stickiness bonus if the previous intent is still reasonable.
pub fn select_intent(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    memory: &AiMemory,
    difficulty: &DifficultyProfile,
) -> TacticalIntent {
    let mut best_score = f32::NEG_INFINITY;
    let mut best_intent: Option<TacticalIntent> = None;

    let mut consider = |intent: TacticalIntent, score: f32| {
        let mut s = score;
        // Stickiness: bonus for continuing the same intent.
        if memory.turns_committed < MAX_COMMITTED_TURNS
            && memory.last_intent == Some(intent.kind())
        {
            s += STICKINESS_BONUS;
            if let (Some(prev), Some(cur)) = (memory.last_target, intent.target()) {
                if prev == cur {
                    s += TARGET_STICKINESS_BONUS;
                }
            }
        }
        if s > best_score {
            best_score = s;
            best_intent = Some(intent);
        }
    };

    let hp_pct = active.hp as f32 / active.max_hp.max(1) as f32;
    let danger = maps.danger.get(active.pos);

    // Hard override: critically wounded in high danger — survival is non-negotiable.
    // Thresholds shift with survival_instinct (HP) and awareness (danger gate):
    // a less-aware AI needs more obvious danger to even trigger the override.
    let hp_panic = difficulty.survival_hp_threshold();
    let danger_panic = difficulty.awareness_danger_threshold();
    if hp_pct < hp_panic && danger > danger_panic {
        return TacticalIntent::ProtectSelf;
    }

    // ProtectSelf: score scales with urgency.
    // danger is normalized [0, 1]; any non-zero danger + low HP triggers.
    if hp_pct < 0.4 && danger > 0.0 {
        let urgency = (1.0 - hp_pct) * danger;
        consider(TacticalIntent::ProtectSelf, urgency);
    }

    // ProtectAlly: score based on ally urgency.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let most_wounded = snap
            .allies_of(active.team)
            .filter(|u| u.entity != active.entity)
            .filter(|u| (u.hp as f32 / u.max_hp.max(1) as f32) < 0.5)
            .min_by_key(|u| u.hp);
        if let Some(ally) = most_wounded {
            let ally_pct = ally.hp as f32 / ally.max_hp.max(1) as f32;
            let urgency = 1.0 - ally_pct; // 0.0..1.0, higher = more wounded
            consider(TacticalIntent::ProtectAlly { ally: ally.entity }, urgency);
        }
    }

    // FocusTarget: killable enemy scores highest, otherwise best priority target.
    // "Killable" uses effective HP (hp + armor) — consistent with the utility
    // `kill` factor. Prevents focusing on a low-HP tank whose armor blocks the hit.
    let killable = snap
        .enemies_of(active.team)
        .filter(|e| {
            let effective_hp = e.hp as f32 + (e.armor + e.armor_bonus) as f32;
            active.threat >= effective_hp
        })
        .min_by_key(|e| e.hp + e.armor + e.armor_bonus);
    if let Some(target) = killable {
        // Killable targets get a high base score.
        let kill_score = 1.2 + (1.0 - target.hp as f32 / target.max_hp.max(1) as f32) * 0.3;
        consider(TacticalIntent::FocusTarget { target: target.entity }, kill_score);
    } else {
        // Fallback: highest-priority target with moderate score.
        let default_target = snap
            .enemies_of(active.team)
            .max_by(|a, b| {
                target_priority(active, a, snap)
                    .partial_cmp(&target_priority(active, b, snap))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(target) = default_target {
            let prio = target_priority(active, target, snap);
            consider(TacticalIntent::FocusTarget { target: target.entity }, 0.5 + prio * 0.3);
        }
    }

    // ApplyCC: high-threat unstunned enemy.
    if active.tags.contains(AiTags::CAN_CC) {
        let cc_target = snap
            .enemies_of(active.team)
            .filter(|e| !e.tags.contains(AiTags::IS_STUNNED))
            .max_by(|a, b| a.threat.partial_cmp(&b.threat).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(target) = cc_target {
            let cc_score = 0.8 + target.threat * 0.1;
            consider(TacticalIntent::ApplyCC { target: target.entity }, cc_score);
        }
    }

    // SetupAOE: enemies clustered.
    if active.tags.contains(AiTags::HAS_AOE) {
        let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
        let cluster_count = enemies.iter().enumerate().filter(|(i, a)| {
            enemies[*i + 1..]
                .iter()
                .any(|b| a.pos.unsigned_distance_to(b.pos) <= 2)
        }).count();
        if cluster_count > 0 {
            let aoe_score = 0.7 + cluster_count as f32 * 0.2;
            consider(TacticalIntent::SetupAOE, aoe_score);
        }
    }

    // Reposition: current position is significantly bad. awareness controls
    // how early the AI notices a bad tile (low = only truly terrible tiles).
    let pos_eval = evaluate_position(active.pos, active.role, maps);
    let repo_threshold = difficulty.awareness_reposition_threshold();
    if pos_eval < repo_threshold {
        let repo_score = 0.3 + (repo_threshold - pos_eval).min(1.5) * 0.4;
        consider(TacticalIntent::Reposition, repo_score);
    }

    best_intent.unwrap_or(TacticalIntent::Reposition)
}

/// Update memory after intent is selected.
pub fn update_memory(memory: &mut AiMemory, intent: &TacticalIntent) {
    let kind = intent.kind();
    let target = intent.target();
    if memory.last_intent == Some(kind) && memory.last_target == target {
        memory.turns_committed = memory.turns_committed.saturating_add(1);
    } else {
        memory.turns_committed = 0;
    }
    memory.last_intent = Some(kind);
    memory.last_target = target;
}

// ── Intent → utility score (factor[7]) ──────────────────────────────────────

/// Compute how well a candidate aligns with the current intent.
/// Positive = aligned, zero = neutral, negative = misaligned (soft penalty).
pub fn intent_score(
    intent: &TacticalIntent,
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    db: &GameDb,
    difficulty: &DifficultyProfile,
) -> f32 {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            if candidate.target == *target {
                return 1.0;
            }
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return MISALIGN_PENALTY;
            };
            // Heals pass through neutrally.
            if def.target_type == TargetType::SingleAlly {
                0.3
            } else {
                MISALIGN_PENALTY
            }
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
            } else if is_cc {
                // CC on wrong target — misaligned.
                MISALIGN_PENALTY
            } else if candidate.target == *target {
                0.5
            } else {
                0.0
            }
        }
        TacticalIntent::Reposition => {
            let current = evaluate_position(active.pos, active.role, maps);
            let new = evaluate_position(candidate.tile, active.role, maps);
            let improvement = new - current;
            let min_improv = difficulty.reposition_min_improvement();
            // Only reward meaningful improvement; penalize lateral/worse moves.
            if improvement < min_improv {
                -1.0
            } else {
                improvement.min(2.0)
            }
        }
        TacticalIntent::ProtectSelf => {
            // Normalized danger [0, 1]: safer tiles score higher.
            let danger = maps.danger.get(candidate.tile);
            1.0 - danger
        }
        TacticalIntent::ProtectAlly { ally } => {
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return 0.0;
            };
            if def.target_type == TargetType::SingleAlly {
                // Heal on the right ally is great, wrong ally is penalized.
                if candidate.target == *ally { 1.0 } else { MILD_PENALTY }
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
                // Single-target when intent says AoE — mild penalty.
                return MILD_PENALTY;
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
        TacticalIntent::LastStand => {
            let Some(def) = db.abilities.get(&candidate.ability) else {
                return 0.0;
            };
            let mut score = 0.0f32;

            // Offensive actions are aligned with LastStand.
            if matches!(def.target_type, TargetType::SingleEnemy) {
                score += 0.5;
            }

            // CC on unstunned target — high value.
            if let Some(target_unit) = snap.unit(candidate.target) {
                let is_cc = def.statuses.iter().any(|sa| {
                    db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn)
                });
                if is_cc && !target_unit.tags.contains(AiTags::IS_STUNNED) {
                    score += 0.8;
                }
            }

            // AoE bonus.
            if def.aoe != AoEShape::None {
                score += 0.3;
            }

            // Heals/buffs are not the priority but not penalized.
            if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                score += 0.1;
            }

            score
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::role::AiRole;
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::utility::ActionCandidate;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::game::resources::GameDb;

    /// Build maps where only danger is set on specific tiles.
    /// Bruiser danger weight is -1.2, so eval = -1.2 * danger.
    fn maps_with_dangers(tiles: &[(Hex, f32)]) -> InfluenceMaps {
        let mut danger = InfluenceMap::new();
        for &(hex, val) in tiles {
            danger.add(hex, val);
        }
        InfluenceMaps {
            danger,
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    fn dummy_unit(pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(0).expect("valid"),
            team: Team::Enemy,
            role: AiRole::Bruiser,
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
            abilities: vec![],
            statuses: vec![],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
        }
    }

    fn dummy_candidate(tile: Hex) -> ActionCandidate {
        ActionCandidate {
            tile,
            path: vec![],
            ability: "melee_attack".into(),
            target_pos: tile,
            target: Entity::from_raw_u32(1).expect("valid"),
        }
    }

    #[test]
    fn reposition_penalizes_worse_tile() {
        // Current pos: eval = -1.2 * 1.5 = -1.8
        // Better tile:  eval = -1.2 * (7/6) ≈ -1.4  (improvement 0.4)
        // Worse tile:   eval = -1.2 * (19/12) ≈ -1.9 (improvement -0.1)
        let current = hex_from_offset(3, 3);
        let better = hex_from_offset(4, 3);
        let worse = hex_from_offset(2, 3);

        let maps = maps_with_dangers(&[
            (current, 1.5),
            (better, 7.0 / 6.0),
            (worse, 19.0 / 12.0),
        ]);

        let active = dummy_unit(current);
        let enemy = UnitSnapshot {
            entity: Entity::from_raw_u32(1).expect("valid"),
            team: Team::Player,
            ..dummy_unit(hex_from_offset(0, 0))
        };
        let snap = BattleSnapshot {
            units: vec![active.clone(), enemy],
            active_unit: active.entity,
            round: 1,
        };
        let db = GameDb::default();
        let intent = TacticalIntent::Reposition;
        let difficulty = DifficultyProfile::default();

        let score_worse = intent_score(
            &intent,
            &dummy_candidate(worse),
            &active,
            &snap,
            &maps,
            &db,
            &difficulty,
        );
        let score_better = intent_score(
            &intent,
            &dummy_candidate(better),
            &active,
            &snap,
            &maps,
            &db,
            &difficulty,
        );

        assert!(
            score_worse < 0.0,
            "worse tile should be penalized, got {score_worse}"
        );
        assert!(
            score_better > 0.0,
            "better tile should score positively, got {score_better}"
        );
    }
}
