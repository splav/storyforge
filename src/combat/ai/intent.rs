use crate::content::content_view::ContentView;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::factors::{aoe_area, aoe_hits};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::{highest_priority_enemy, target_priority};
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::planning::types::TurnPlan;
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::fmt;

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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum TacticalIntent {
    /// Focus fire: kill or heavily damage a specific target.
    FocusTarget {
        #[serde(with = "crate::combat::ai::serde_helpers::entity")]
        target: Entity,
    },
    /// Apply CC (stun) to a high-threat target.
    ApplyCC {
        #[serde(with = "crate::combat::ai::serde_helpers::entity")]
        target: Entity,
    },
    /// Reposition to a better tile.
    Reposition,
    /// Self-preservation: avoid danger.
    ProtectSelf,
    /// Protect/heal a specific wounded ally.
    ProtectAlly {
        #[serde(with = "crate::combat::ai::serde_helpers::entity")]
        ally: Entity,
    },
    /// Position to hit multiple enemies with AoE.
    SetupAOE,
    /// Survival is unlikely — maximize last useful action (kill > cc > damage).
    LastStand,
}

/// Intent kind without target data, for stickiness comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
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

// ── Intent selection reason ────────────────────────────────────────────────

/// Structured explanation for why a given intent was picked.
///
/// Emitted at the decision site — producer fills the variant's fields directly
/// so the log/overlay never re-parse a freetext string. Each variant maps to a
/// stable `code()` for the JSONL analyzer and a `Display` impl for human text.
///
/// Add a new rule by adding a variant here and emitting it at the rule site.
/// Classification (`selection_kind` in the log) is compiler-checked via
/// `code()` — there is no string-prefix table to keep in sync.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntentReason {
    PanicOverride { hp_pct: f32, hp_threshold: f32, danger: f32, danger_threshold: f32 },
    Urgency { hp_pct: f32, danger: f32 },
    ProtectAlly { ally_hp_pct: f32, threshold: f32, heal_identity: f32 },
    TauntForced,
    TauntCc { dpr: f32 },
    Killable { threat: f32, eff_hp: i32, reach_budget: u32 },
    BestPriority { priority: f32 },
    ApplyCc { dpr: f32 },
    SetupAoe { clustered_pairs: usize },
    Reposition { pos_eval: f32, threshold: f32 },
    NoRuleDefault,
    MidpanicFallback {
        hp_pct: f32,
        midpanic_hp: f32,
        danger: f32,
        panic_danger: f32,
        max_align: f32,
        threshold: f32,
    },
    ViabilityFallback {
        from: IntentKind,
        max_align: f32,
        threshold: f32,
    },
    /// ProtectSelf mask found no defensive plan and rescored under LastStand.
    /// Boxed so the enum stays small; `prior` is always a non-LastStand
    /// reason (see `pick_action`).
    LastStandAfter { prior: Box<IntentReason> },
}

impl IntentReason {
    /// Stable snake_case code for analyzers. The JSONL log stores this as
    /// `selection_kind`. Must stay backward-compatible — rename requires
    /// bumping `log::SCHEMA_VERSION`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PanicOverride { .. } => "panic_override",
            Self::Urgency { .. } => "urgency",
            Self::ProtectAlly { .. } => "protect_ally",
            Self::TauntForced => "taunt_forced",
            Self::TauntCc { .. } => "taunt_cc",
            Self::Killable { .. } => "killable",
            Self::BestPriority { .. } => "best_priority",
            Self::ApplyCc { .. } => "apply_cc",
            Self::SetupAoe { .. } => "setup_aoe",
            Self::Reposition { .. } => "reposition",
            Self::NoRuleDefault => "no_rule_default",
            Self::MidpanicFallback { .. } => "midpanic_fallback",
            Self::ViabilityFallback { .. } => "viability_fallback",
            Self::LastStandAfter { .. } => "last_stand_after",
        }
    }
}

impl fmt::Display for IntentReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PanicOverride { hp_pct, hp_threshold, danger, danger_threshold } => write!(
                f, "panic: hp%={:.0}%<{:.0}% AND danger={:.2}>{:.2}",
                hp_pct * 100.0, hp_threshold * 100.0, danger, danger_threshold,
            ),
            Self::Urgency { hp_pct, danger } => write!(
                f, "hp%={:.0}%<40% × danger={:.2}", hp_pct * 100.0, danger,
            ),
            Self::ProtectAlly { ally_hp_pct, threshold, heal_identity } => write!(
                f, "ally hp%={:.0}%<{:.0}% (healer support={:.2})",
                ally_hp_pct * 100.0, threshold * 100.0, heal_identity,
            ),
            Self::TauntForced => write!(f, "forced by taunt (FORCES_TARGETING)"),
            Self::TauntCc { dpr } => write!(f, "CC the taunter (dpr={:.1})", dpr),
            Self::Killable { threat, eff_hp, reach_budget } => write!(
                f, "killable: threat={:.1}>=eff_hp={}, reach_budget={}",
                threat, eff_hp, reach_budget,
            ),
            Self::BestPriority { priority } => write!(f, "highest priority={:.2}", priority),
            Self::ApplyCc { dpr } => write!(f, "unstunned enemy dpr={:.1}", dpr),
            Self::SetupAoe { clustered_pairs } => write!(
                f, "{} clustered enemy pair(s) within dist≤2", clustered_pairs,
            ),
            Self::Reposition { pos_eval, threshold } => write!(
                f, "pos_eval={:.2} < awareness_threshold={:.2}", pos_eval, threshold,
            ),
            Self::NoRuleDefault => write!(f, "no rule matched — default reposition"),
            Self::MidpanicFallback {
                hp_pct, midpanic_hp, danger, panic_danger, max_align, threshold,
            } => write!(
                f,
                "midpanic_fallback: hp%={:.0}%<{:.0}% AND danger={:.2}>{:.2} (max_align={:.2}<{:.2})",
                hp_pct * 100.0, midpanic_hp * 100.0, danger, panic_danger, max_align, threshold,
            ),
            Self::ViabilityFallback { from, max_align, threshold } => write!(
                f, "fallback from {:?}: max_align={:.2}<threshold={:.2}",
                from, max_align, threshold,
            ),
            Self::LastStandAfter { prior } => write!(
                f, "{} → LastStand (no defensive plan)", prior,
            ),
        }
    }
}

// ── Intent selection (scored + hysteresis) ──────────────────────────────────

/// Result of intent selection. `reason` captures the actual numbers that made
/// this rule fire — built inline at decision time so a future threshold tweak
/// in `difficulty.rs` can't desync from the logged explanation.
pub struct IntentChoice {
    pub intent: TacticalIntent,
    pub reason: IntentReason,
}

/// Analyze the battlefield, score all valid intents, and pick the best.
/// Applies stickiness bonus if the previous intent is still reasonable.
pub fn select_intent(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    memory: &AiMemory,
    difficulty: &DifficultyProfile,
) -> IntentChoice {
    let mut best_score = f32::NEG_INFINITY;
    let mut best: Option<IntentChoice> = None;

    let mut consider = |intent: TacticalIntent, score: f32, reason: IntentReason| {
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
            best = Some(IntentChoice { intent, reason });
        }
    };

    let hp_pct = active.hp_pct();
    let danger = maps.danger.get(active.pos);

    // Hard override: critically wounded in high danger — survival is non-negotiable.
    // Thresholds shift with survival_instinct (HP) and awareness (danger gate):
    // a less-aware AI needs more obvious danger to even trigger the override.
    let hp_panic = difficulty.survival_hp_threshold();
    let danger_panic = difficulty.awareness_danger_threshold();
    if hp_pct < hp_panic && danger > danger_panic {
        return IntentChoice {
            intent: TacticalIntent::ProtectSelf,
            reason: IntentReason::PanicOverride {
                hp_pct,
                hp_threshold: hp_panic,
                danger,
                danger_threshold: danger_panic,
            },
        };
    }

    // ProtectSelf: score scales with urgency.
    // danger is normalized [0, 1]; any non-zero danger + low HP triggers.
    if hp_pct < 0.4 && danger > 0.0 {
        let urgency = (1.0 - hp_pct) * danger;
        consider(
            TacticalIntent::ProtectSelf,
            urgency,
            IntentReason::Urgency { hp_pct, danger },
        );
    }

    // ProtectAlly: score based on ally urgency. Self is a valid target.
    //
    // Trigger threshold scales with the actor's healer identity (Support axis):
    // pure damage dealer (support=0) keeps 50% threshold (barely triggers),
    // pure healer (support=1.0) triggers at 70% (aggressive preventive heal).
    // Hybrid battle-mages with heal enter healer-mode proportionally earlier.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let heal_identity = active.role.support.min(1.0);
        let threshold = 0.5 + heal_identity * 0.2;
        let most_wounded = snap
            .allies_of(active.team)
            .filter(|u| u.hp_pct() < threshold)
            .min_by_key(|u| u.hp);
        if let Some(ally) = most_wounded {
            let ally_pct = ally.hp_pct();
            let urgency = 1.0 - ally_pct;
            consider(
                TacticalIntent::ProtectAlly { ally: ally.entity },
                urgency,
                IntentReason::ProtectAlly {
                    ally_hp_pct: ally_pct,
                    threshold,
                    heal_identity,
                },
            );
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
        consider(
            TacticalIntent::FocusTarget { target: t.entity },
            1.2,
            IntentReason::TauntForced,
        );
        if active.tags.contains(AiTags::CAN_CC) && !t.tags.contains(AiTags::IS_STUNNED) {
            // Intent score uses horizon-average (DPR) rather than peak
            // `threat` so CC-ing a burst mage with empty mana doesn't
            // over-commit the planner; a sustained fighter still scores
            // high. Constants unchanged.
            let dpr = crate::combat::ai::scoring::horizon_avg(t);
            consider(
                TacticalIntent::ApplyCC { target: t.entity },
                0.8 + dpr * 0.1,
                IntentReason::TauntCc { dpr },
            );
        }
    } else {
        // FocusTarget: killable enemy scores highest, otherwise best priority target.
        // "Killable" requires BOTH: (a) effective HP within threat (armor-aware),
        // (b) reachable this turn (dist ≤ speed + max attack range).
        let reach_budget = (active.speed.max(0) as u32).saturating_add(active.max_attack_range);
        let killable = snap
            .enemies_of(active.team)
            .filter(|e| active.threat >= e.eff_hp() as f32)
            .filter(|e| active.pos.unsigned_distance_to(e.pos) <= reach_budget)
            .min_by_key(|e| e.eff_hp());
        if let Some(target) = killable {
            let kill_score = 1.2 + (1.0 - target.hp_pct()) * 0.3;
            consider(
                TacticalIntent::FocusTarget { target: target.entity },
                kill_score,
                IntentReason::Killable {
                    threat: active.threat,
                    eff_hp: target.eff_hp(),
                    reach_budget,
                },
            );
        } else if let Some(target) = highest_priority_enemy(active, snap) {
            let prio = target_priority(active, target, snap);
            consider(
                TacticalIntent::FocusTarget { target: target.entity },
                0.5 + prio * 0.3,
                IntentReason::BestPriority { priority: prio },
            );
        }

        // ApplyCC: high-sustained-damage unstunned enemy.
        if active.tags.contains(AiTags::CAN_CC) {
            // Rank by DPR (horizon-average) so the CC intent targets who
            // actually contributes the most over the combat window —
            // burst casters with empty pools drop relative to sustained
            // fighters, matching the stun-value scoring downstream.
            let cc_target = snap
                .enemies_of(active.team)
                .filter(|e| !e.tags.contains(AiTags::IS_STUNNED))
                .max_by(|a, b| {
                    let da = crate::combat::ai::scoring::horizon_avg(a);
                    let db = crate::combat::ai::scoring::horizon_avg(b);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(target) = cc_target {
                let dpr = crate::combat::ai::scoring::horizon_avg(target);
                let cc_score = 0.8 + dpr * 0.1;
                consider(
                    TacticalIntent::ApplyCC { target: target.entity },
                    cc_score,
                    IntentReason::ApplyCc { dpr },
                );
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
            consider(
                TacticalIntent::SetupAOE,
                aoe_score,
                IntentReason::SetupAoe { clustered_pairs: cluster_count },
            );
        }
    }

    // Reposition: current position is significantly bad. awareness controls
    // how early the AI notices a bad tile (low = only truly terrible tiles).
    let pos_eval = evaluate_position(active.pos, &active.role, maps);
    let repo_threshold = difficulty.awareness_reposition_threshold();
    if pos_eval < repo_threshold {
        let repo_score = 0.3 + (repo_threshold - pos_eval).min(1.5) * 0.4;
        consider(
            TacticalIntent::Reposition,
            repo_score,
            IntentReason::Reposition { pos_eval, threshold: repo_threshold },
        );
    }

    best.unwrap_or(IntentChoice {
        intent: TacticalIntent::Reposition,
        reason: IntentReason::NoRuleDefault,
    })
}

/// Minimum `intent_score` value indicating the intent can actually be executed
/// by *some* candidate. If nothing reaches this threshold, the intent is moot
/// and pick_action swaps to a FocusTarget default to avoid stale commitments
/// (e.g., AI declares "Reposition" but every tile is worse than staying).
///
/// Returns `None` for intents with dedicated flows in `pick_action`
/// (`ProtectSelf`, `LastStand`) — the viability guard is simply skipped for
/// those.
pub fn intent_viability_threshold(intent: &TacticalIntent) -> Option<f32> {
    match intent {
        // Need an actual improvement to call it repositioning.
        TacticalIntent::Reposition => Some(0.01),
        // Intent factor is a discounted sum (see scorer module doc).
        // A plan with at least one Cast on the focus enemy accumulates
        // intent_sum of ~0.72 (deep-3) up to 1.0 (direct cast). Any plan
        // without a focus-targeting Cast sits at 0 or negative. Threshold
        // 0.5 accepts the approach-and-strike trajectory while still
        // trapping "no reachable focus target at all" cases.
        TacticalIntent::FocusTarget { .. } => Some(0.5),
        // CC match contributes 1.0 (direct) down to 0.72 (deep); damage
        // on the CC target contributes 0.5 — threshold 0.5 accepts any
        // committed CC attempt including bundled and damage-on-target.
        TacticalIntent::ApplyCC { .. } => Some(0.5),
        // Heal on the right ally is 1.0 (direct), 0.85 bundled, 0.72
        // deep. Threshold 0.5 accepts the approach-and-heal line.
        TacticalIntent::ProtectAlly { .. } => Some(0.5),
        // Any AoE hit fraction > 0 counts.
        TacticalIntent::SetupAOE => Some(0.01),
        TacticalIntent::ProtectSelf | TacticalIntent::LastStand => None,
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
    plans: &[TurnPlan],
    actor_pos: Hex,
    exclude: Option<Entity>,
) -> Option<Entity> {
    // A plan's "reachable target" is the target of its committed prefix —
    // matches what the actor would actually hit this tick.
    let reachable: std::collections::HashSet<Entity> = plans
        .iter()
        .filter_map(|p| ScoredStep::from_plan_committed(p, actor_pos).target())
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

/// Compute how well a scored step aligns with the current intent.
/// Positive = aligned, zero = neutral, negative = misaligned (soft penalty).
pub fn intent_score(
    intent: &TacticalIntent,
    step: &ScoredStep,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    content: &ContentView,
    difficulty: &DifficultyProfile,
) -> f32 {
    // Move steps: scored only on position-related intent axes.
    let cast = match step {
        ScoredStep::Cast { ability, target_pos, target, .. } => {
            Some((*ability, *target_pos, *target))
        }
        ScoredStep::Move { .. } => None,
    };

    match intent {
        TacticalIntent::FocusTarget { target: focus } => match cast {
            Some((ability, _, target)) => {
                if target == *focus {
                    return 1.0;
                }
                let Some(def) = content.abilities.get(ability) else {
                    return MISALIGN_PENALTY;
                };
                // AoE that covers the focus target: partial alignment — the
                // area catches the focus even without naming it.
                if def.aoe != AoEShape::None {
                    if let Some(focus_unit) = snap.unit(*focus) {
                        if let ScoredStep::Cast { target_pos, caster_tile, .. } = step {
                            let area = aoe_area(def, *target_pos, *caster_tile);
                            if area.contains(&focus_unit.pos) {
                                return 0.6;
                            }
                        }
                    }
                    return MISALIGN_PENALTY;
                }
                if def.target_type == TargetType::SingleAlly { 0.3 } else { MISALIGN_PENALTY }
            }
            // Pure move during FocusTarget is neutral — not aligned, not punished.
            None => 0.0,
        },
        TacticalIntent::ApplyCC { target: cc_target } => match cast {
            Some((ability, _, target)) => {
                let Some(def) = content.abilities.get(ability) else {
                    return 0.0;
                };
                let is_cc = applies_cc(def, content);
                if is_cc && target == *cc_target { 1.0 }
                else if is_cc { MISALIGN_PENALTY }
                else if target == *cc_target { 0.5 }
                else { 0.0 }
            }
            None => 0.0,
        },
        TacticalIntent::Reposition => {
            // Tiered: strong improvement rewarded, any improvement neutral,
            // no improvement penalized — mildly if casting, hard if just moving.
            let current = evaluate_position(active.pos, &active.role, maps);
            let new = evaluate_position(step.caster_tile(), &active.role, maps);
            let improvement = new - current;
            let min_improv = difficulty.reposition_min_improvement();
            if improvement >= min_improv {
                improvement.min(2.0)
            } else if improvement > 0.0 {
                0.0
            } else if cast.is_some() {
                -0.3
            } else {
                -1.0
            }
        }
        TacticalIntent::ProtectSelf => {
            // Self-directed defensive casts (self-heal, self-buff on Myself or
            // SingleAlly aimed at caster) are full ProtectSelf alignment —
            // staying put to save yourself is protecting self, regardless of
            // tile danger. Otherwise use tile safety.
            if let Some((ability, _, target)) = cast {
                if target == active.entity {
                    if let Some(def) = content.abilities.get(ability) {
                        if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                            return 1.0;
                        }
                    }
                }
            }
            1.0 - maps.danger.get(step.caster_tile())
        }
        TacticalIntent::ProtectAlly { ally } => match cast {
            Some((ability, _, target)) => {
                let Some(def) = content.abilities.get(ability) else { return 0.0 };
                if def.target_type == TargetType::SingleAlly {
                    if target == *ally { 1.0 } else { MILD_PENALTY }
                } else if snap.unit(*ally).is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1) {
                    0.5
                } else {
                    0.0
                }
            }
            // Move adjacent to the wounded ally = mild support (bodyguard).
            None => {
                if snap.unit(*ally).is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1) {
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
            let Some(def) = content.abilities.get(ability) else { return 0.0 };
            if def.aoe == AoEShape::None {
                return MILD_PENALTY;
            }
            let area = aoe_area(def, target_pos, step.caster_tile());
            let total = snap.enemies_of(active.team).count() as f32;
            let hit = aoe_hits(&area, active, snap).enemies.len() as f32;
            if total > 0.0 { hit / total } else { 0.0 }
        }
        TacticalIntent::LastStand => {
            let Some((ability, _, target)) = cast else {
                // LastStand wants last useful action, not running.
                return -0.3;
            };
            let Some(def) = content.abilities.get(ability) else { return 0.0 };
            let mut score = 0.0f32;

            // "Direct offensive action" bonus in LastStand: covers both
            // entity-targeted (SingleEnemy) and cell-targeted (Ground)
            // attacks. AoE footprint gets an additional +0.3 below.
            if matches!(def.target_type, TargetType::SingleEnemy | TargetType::Ground) {
                score += 0.5;
            }
            if let Some(target_unit) = snap.unit(target) {
                if applies_cc(def, content) && !target_unit.tags.contains(AiTags::IS_STUNNED) {
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
    use crate::combat::ai::influence::InfluenceMaps;
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{empty_maps, UnitBuilder};
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};

    /// Danger-only maps for intent-scoring tests; other three maps stay
    /// empty. Reposition scoring keys off `evaluate_position`, which reads
    /// danger with the Bruiser weight of -1.2 (so eval = -1.2 × danger).
    fn maps_with_dangers(tiles: &[(Hex, f32)]) -> InfluenceMaps {
        let mut m = empty_maps();
        for &(hex, val) in tiles {
            m.danger.add(hex, val);
        }
        m
    }

    fn dummy_unit(pos: Hex) -> UnitSnapshot {
        UnitBuilder::new(0, Team::Enemy, pos)
            .tags(AiTags::MELEE_ONLY)
            .build()
    }

    /// Caller owns the `AbilityId` so the `ScoredStep` ref stays valid for
    /// the scope of the test.
    fn dummy_step<'a>(tile: Hex, ability: &'a AbilityId) -> ScoredStep<'a> {
        ScoredStep::Cast {
            ability,
            target: Entity::from_raw_u32(1).expect("valid"),
            target_pos: tile,
            caster_tile: tile,
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
        let snap = BattleSnapshot::new(vec![active.clone(), enemy], 1);
        let content = ContentView::load_global_for_tests();
        let intent = TacticalIntent::Reposition;
        let difficulty = DifficultyProfile::default();

        let ab = AbilityId::from("melee_attack");
        let score_worse = intent_score(
            &intent,
            &dummy_step(worse, &ab),
            &active,
            &snap,
            &maps,
            &content,
            &difficulty,
        );
        let score_better = intent_score(
            &intent,
            &dummy_step(better, &ab),
            &active,
            &snap,
            &maps,
            &content,
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
