use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::{ActionCandidate, CandidateKind};
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    // ProtectAlly: score based on ally urgency. Self is a valid target — a
    // wounded healer should pick themselves if they're the most at risk.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let most_wounded = snap
            .allies_of(active.team)
            .filter(|u| (u.hp as f32 / u.max_hp.max(1) as f32) < 0.5)
            .min_by_key(|u| u.hp);
        if let Some(ally) = most_wounded {
            let ally_pct = ally.hp as f32 / ally.max_hp.max(1) as f32;
            let urgency = 1.0 - ally_pct; // 0.0..1.0, higher = more wounded
            consider(TacticalIntent::ProtectAlly { ally: ally.entity }, urgency);
        }
    }

    // Taunt: if an enemy has FORCES_TARGETING, engine filters all Cast-candidates
    // to that enemy only. Restrict FocusTarget/ApplyCC to the taunter so we don't
    // pick an unreachable "priority" target and then fall back through the viability
    // guard — that produced confusing "Priority target: X … fallback to Y" logs.
    let taunter = snap.enemies_of(active.team)
        .find(|e| e.tags.contains(AiTags::FORCES_TARGETING));

    if let Some(t) = taunter {
        // Forced engagement. Score on par with killable so it beats default FocusTarget
        // but can still lose to ProtectSelf/ProtectAlly in a survival crisis.
        consider(TacticalIntent::FocusTarget { target: t.entity }, 1.2);
        if active.tags.contains(AiTags::CAN_CC) && !t.tags.contains(AiTags::IS_STUNNED) {
            consider(TacticalIntent::ApplyCC { target: t.entity }, 0.8 + t.threat * 0.1);
        }
    } else {
        // FocusTarget: killable enemy scores highest, otherwise best priority target.
        // "Killable" requires BOTH: (a) effective HP within threat (armor-aware),
        // (b) reachable this turn (dist ≤ speed + max attack range).
        let reach_budget = (active.speed.max(0) as u32).saturating_add(active.max_attack_range);
        let killable = snap
            .enemies_of(active.team)
            .filter(|e| {
                let effective_hp = e.hp as f32 + (e.armor + e.armor_bonus) as f32;
                active.threat >= effective_hp
            })
            .filter(|e| active.pos.unsigned_distance_to(e.pos) <= reach_budget)
            .min_by_key(|e| e.hp + e.armor + e.armor_bonus);
        if let Some(target) = killable {
            let kill_score = 1.2 + (1.0 - target.hp as f32 / target.max_hp.max(1) as f32) * 0.3;
            consider(TacticalIntent::FocusTarget { target: target.entity }, kill_score);
        } else {
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

/// Minimum `intent_score` value indicating the intent can actually be executed
/// by *some* candidate. If nothing reaches this threshold, the intent is moot
/// and pick_action swaps to a FocusTarget default to avoid stale commitments
/// (e.g., AI declares "Reposition" but every tile is worse than staying).
pub fn intent_viability_threshold(intent: &TacticalIntent) -> f32 {
    match intent {
        // Need an actual improvement to call it repositioning.
        TacticalIntent::Reposition => 0.01,
        // Must have a candidate that actually targets the focus enemy.
        TacticalIntent::FocusTarget { .. } => 1.0,
        // At least damage on the CC target (0.5) — full CC match is 1.0.
        TacticalIntent::ApplyCC { .. } => 0.5,
        // Heal on the right ally is 1.0; adjacent-tile fallback is 0.5.
        TacticalIntent::ProtectAlly { .. } => 0.5,
        // Any AoE hit fraction > 0 counts.
        TacticalIntent::SetupAOE => 0.01,
        // These have dedicated flows / fallbacks inside pick_action.
        TacticalIntent::ProtectSelf | TacticalIntent::LastStand => f32::NEG_INFINITY,
    }
}

/// Pick a fallback FocusTarget.
///
/// Preference order:
/// 1. Enemy that at least one candidate can actually reach this turn (highest priority among them).
/// 2. If no candidate reaches any enemy, highest-priority enemy overall — so AI commits
///    to a direction even when no move lands this turn.
///
/// `exclude` skips the original unreachable target so we pick a genuinely
/// different fallback (avoids "fallback from FocusTarget(X) → FocusTarget(X)").
pub fn default_focus_target(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    candidates: &[ActionCandidate],
    exclude: Option<Entity>,
) -> Option<Entity> {
    let reachable: std::collections::HashSet<Entity> = candidates
        .iter()
        .filter_map(|c| c.target())
        .collect();

    let pick_best = |include_reachable_only: bool| {
        snap.enemies_of(active.team)
            .filter(|e| Some(e.entity) != exclude)
            .filter(|e| !include_reachable_only || reachable.contains(&e.entity))
            .max_by(|a, b| {
                target_priority(active, a, snap)
                    .partial_cmp(&target_priority(active, b, snap))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| e.entity)
    };

    pick_best(true).or_else(|| pick_best(false))
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
    // MoveOnly candidates: scored only on position-related intent axes.
    let cast = match &candidate.kind {
        CandidateKind::Cast { ability, target_pos, target } => {
            Some((ability, *target_pos, *target))
        }
        CandidateKind::MoveOnly => None,
    };

    match intent {
        TacticalIntent::FocusTarget { target: focus } => match cast {
            Some((ability, _, target)) => {
                if target == *focus {
                    return 1.0;
                }
                let Some(def) = db.abilities.get(ability) else {
                    return MISALIGN_PENALTY;
                };
                if def.target_type == TargetType::SingleAlly { 0.3 } else { MISALIGN_PENALTY }
            }
            // Pure move during FocusTarget is neutral — not aligned, not punished.
            None => 0.0,
        },
        TacticalIntent::ApplyCC { target: cc_target } => match cast {
            Some((ability, _, target)) => {
                let Some(def) = db.abilities.get(ability) else {
                    return 0.0;
                };
                let is_cc = def.statuses.iter().any(|sa| {
                    db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn)
                });
                if is_cc && target == *cc_target { 1.0 }
                else if is_cc { MISALIGN_PENALTY }
                else if target == *cc_target { 0.5 }
                else { 0.0 }
            }
            None => 0.0,
        },
        TacticalIntent::Reposition => {
            // Same formula for Cast and MoveOnly — position improvement is the axis.
            let current = evaluate_position(active.pos, active.role, maps);
            let new = evaluate_position(candidate.tile, active.role, maps);
            let improvement = new - current;
            let min_improv = difficulty.reposition_min_improvement();
            if improvement < min_improv { -1.0 } else { improvement.min(2.0) }
        }
        TacticalIntent::ProtectSelf => {
            // Self-directed defensive casts (self-heal, self-buff on Myself or
            // SingleAlly aimed at caster) are full ProtectSelf alignment —
            // staying put to save yourself is protecting self, regardless of
            // tile danger. Otherwise use tile safety.
            if let Some((ability, _, target)) = cast {
                if target == active.entity {
                    if let Some(def) = db.abilities.get(ability) {
                        if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                            return 1.0;
                        }
                    }
                }
            }
            1.0 - maps.danger.get(candidate.tile)
        }
        TacticalIntent::ProtectAlly { ally } => match cast {
            Some((ability, _, target)) => {
                let Some(def) = db.abilities.get(ability) else { return 0.0 };
                if def.target_type == TargetType::SingleAlly {
                    if target == *ally { 1.0 } else { MILD_PENALTY }
                } else if snap.unit(*ally).is_some_and(|a| candidate.tile.unsigned_distance_to(a.pos) <= 1) {
                    0.5
                } else {
                    0.0
                }
            }
            // Move adjacent to the wounded ally = mild support (bodyguard).
            None => {
                if snap.unit(*ally).is_some_and(|a| candidate.tile.unsigned_distance_to(a.pos) <= 1) {
                    0.5
                } else {
                    0.0
                }
            }
        },
        TacticalIntent::SetupAOE => {
            let Some((ability, target_pos, _)) = cast else {
                // Pure movement can't set up AoE; neutral.
                return 0.0;
            };
            let Some(def) = db.abilities.get(ability) else { return 0.0 };
            if def.aoe == AoEShape::None {
                return MILD_PENALTY;
            }
            let area: Vec<Hex> = match def.aoe {
                AoEShape::Circle { radius } => hex_circle(target_pos, radius),
                AoEShape::Line { length } => hex_line(candidate.tile, target_pos, length),
                AoEShape::None => vec![],
            };
            let area_set: HashSet<Hex> = area.into_iter().collect();
            let total = snap.enemies_of(active.team).count() as f32;
            let hit = snap.enemies_of(active.team).filter(|e| area_set.contains(&e.pos)).count() as f32;
            if total > 0.0 { hit / total } else { 0.0 }
        }
        TacticalIntent::LastStand => {
            let Some((ability, _, target)) = cast else {
                // LastStand wants last useful action, not running.
                return -0.3;
            };
            let Some(def) = db.abilities.get(ability) else { return 0.0 };
            let mut score = 0.0f32;

            if matches!(def.target_type, TargetType::SingleEnemy) {
                score += 0.5;
            }
            if let Some(target_unit) = snap.unit(target) {
                let is_cc = def.statuses.iter().any(|sa| {
                    db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn)
                });
                if is_cc && !target_unit.tags.contains(AiTags::IS_STUNNED) {
                    score += 0.8;
                }
            }
            if def.aoe != AoEShape::None {
                score += 0.3;
            }
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
            max_attack_range: 1,
        }
    }

    fn dummy_candidate(tile: Hex) -> ActionCandidate {
        ActionCandidate {
            tile,
            path: vec![],
            kind: CandidateKind::Cast {
                ability: "melee_attack".into(),
                target_pos: tile,
                target: Entity::from_raw_u32(1).expect("valid"),
            },
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
