use crate::content::content_view::ContentView;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::factors::{aoe_area, aoe_hits, compute_factors, PlanFactors};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{ActiveStatusView, AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::{highest_priority_enemy, target_priority};
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::tuning::AiTuning;
use crate::combat::ai::utility::ScoringCtx;
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::fmt;

// ── Intent enum ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
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

// ── Plan freeze: stored plan + invalidation snapshot ──────────────────────

/// World state captured when a MoveOnly plan step is committed. Compared with
/// current state on the next tick to detect events (AoO, status changes, target
/// death/movement) that would make the stored plan stale.
#[derive(Debug, Clone)]
pub struct PlanSnapshot {
    pub actor_hp: i32,
    /// Current rage value (+1 from AoO — reliable AoO signal without needing
    /// separate event tracking).
    pub actor_rage: i32,
    /// Stable hash over active status ids + remaining durations. Changes when
    /// a status is applied, removed, or ticked down (debuffs, auras, etc.).
    pub actor_status_hash: u64,
    /// Where the actor should be on the next tick (destination of the Move).
    pub expected_actor_pos: Hex,
    /// Intent target at plan time, if any (FocusTarget / ApplyCC / ProtectAlly).
    pub target: Option<Entity>,
    pub target_hp: i32,
    pub target_pos: Hex,
}

impl PlanSnapshot {
    pub fn capture(
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
        expected_actor_pos: Hex,
    ) -> Self {
        Self {
            actor_hp: actor.hp,
            actor_rage: actor.rage.map(|(r, _)| r).unwrap_or(0),
            actor_status_hash: status_hash(&actor.statuses),
            expected_actor_pos,
            target: target.map(|t| t.entity),
            target_hp: target.map(|t| t.hp).unwrap_or(0),
            target_pos: target.map(|t| t.pos).unwrap_or_default(),
        }
    }

    /// Returns `None` when the snapshot still matches current world state, or
    /// `Some(reason_code)` identifying the first detected change.
    pub fn mismatch(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
    ) -> Option<&'static str> {
        if actor.pos != self.expected_actor_pos {
            return Some("actor_pos_mismatch");
        }
        if actor.hp < self.actor_hp {
            return Some("actor_hp_drop");
        }
        if actor.rage.map(|(r, _)| r).unwrap_or(0) != self.actor_rage {
            return Some("actor_rage_changed");
        }
        if status_hash(&actor.statuses) != self.actor_status_hash {
            return Some("actor_status_changed");
        }
        if let Some(expected) = self.target {
            match target {
                None => return Some("target_gone"),
                Some(t) => {
                    if t.entity != expected {
                        return Some("target_entity_changed");
                    }
                    if t.hp < self.target_hp {
                        return Some("target_hp_drop");
                    }
                    if t.pos != self.target_pos {
                        return Some("target_moved");
                    }
                }
            }
        }
        None
    }
}

fn status_hash(statuses: &[ActiveStatusView]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    // Sort by id for a deterministic hash regardless of application order.
    let mut pairs: Vec<_> = statuses.iter().map(|s| (&s.id, s.rounds_remaining)).collect();
    pairs.sort_by_key(|(id, _)| id.0.as_str());
    for (id, rounds) in pairs {
        id.hash(&mut h);
        rounds.hash(&mut h);
    }
    h.finish()
}

/// Trimmed plan persisted across ticks during a MoveOnly commitment. Holds the
/// steps and enough context to (a) continue the plan without replanning and (b)
/// compare against the shadow fresh plan for divergence diagnostics.
///
/// `sim_snapshots` are intentionally excluded — they're heavy and only needed
/// during the scoring pass that already completed.
#[derive(Debug, Clone)]
pub struct StoredPlan {
    pub steps: Vec<PlanStep>,
    /// Index of the next step to execute (1 after the first Move committed).
    pub step_index: usize,
    pub snapshot: PlanSnapshot,
    pub intent: IntentKind,
    /// Ability from step[step_index] if it's a Cast, for divergence reporting.
    pub cast_ability: Option<AbilityId>,
    /// Target from step[step_index] if it's a Cast.
    pub cast_target: Option<Entity>,
    /// Final score of the plan at construction time (pre-continuation).
    pub score: f32,
}

// ── Persistent AI memory ───────────────────────────────────────────────────

#[derive(Component, Default)]
pub struct AiMemory {
    pub last_intent: Option<IntentKind>,
    pub last_target: Option<Entity>,
    pub turns_committed: u8,
    /// Stored plan from the previous MoveOnly tick, if any. Cleared when a
    /// non-Move decision fires (Cast, EndTurn) or when the snapshot is stale.
    pub last_plan: Option<StoredPlan>,
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
    /// ADAPTATION switched the chosen plan's evaluation regime. `prior`
    /// is the reason that originally selected the global intent; `reason`
    /// is the fact that triggered the adaptation (per-plan ExpectedSelfLethal
    /// or global ProtectSelfNoDefensive). Boxed so the enum stays small.
    Adapted {
        prior: Box<IntentReason>,
        reason: crate::combat::ai::planning::AdaptationReason,
    },
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
            Self::Adapted { reason, .. } => reason.code(),
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
            Self::Adapted { prior, reason } => {
                use crate::combat::ai::planning::AdaptationReason;
                match reason {
                    AdaptationReason::ExpectedSelfLethal { aoo_dmg, actor_hp } => write!(
                        f,
                        "{} → LastStand (EV-lethal: aoo={:.1} ≥ hp={})",
                        prior, aoo_dmg, actor_hp,
                    ),
                    AdaptationReason::ProtectSelfNoDefensive => write!(
                        f, "{} → LastStand (no defensive plan)", prior,
                    ),
                    AdaptationReason::ProtectSelfFutile { pending_dot, actor_hp } => write!(
                        f,
                        "{} → LastStand (doomed: pending_dot={} ≥ hp={})",
                        prior, pending_dot, actor_hp,
                    ),
                }
            }
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
    tuning: &AiTuning,
) -> IntentChoice {
    let t = &tuning.thresholds;
    let mut best_score = f32::NEG_INFINITY;
    let mut best: Option<IntentChoice> = None;

    let mut consider = |intent: TacticalIntent, score: f32, reason: IntentReason| {
        let mut s = score;
        // Stickiness: bonus for continuing the same intent.
        if memory.turns_committed < t.max_committed_turns
            && memory.last_intent == Some(intent.kind())
        {
            s += t.stickiness_bonus;
            if let (Some(prev), Some(cur)) = (memory.last_target, intent.target()) {
                if prev == cur {
                    s += t.target_stickiness_bonus;
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
            .filter(|_| active.action_points > 0)
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
        // A plan with at least one Cast on the focus enemy produces a
        // positive dot-product of damage/kill factors. A Move that enters
        // the engagement reach scores 0.8. Threshold 0.5 accepts the
        // approach-and-strike trajectory while still trapping "no reachable
        // focus target at all" cases.
        TacticalIntent::FocusTarget { .. } => Some(0.5),
        // CC-on-target scores via cc×1.5 dot product; damage-on-target via
        // damage×0.3. A Move entering CC reach scores 0.8. Threshold 0.5
        // accepts committed CC attempt including approach-and-cc lines.
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

// ── Pursuit (Move alignment under FocusTarget / ApplyCC) ───────────────────

/// Score a pure Move step by how much it closes the gap to the intent's
/// target, with an explicit reward for entering a "threat bubble" from
/// which the actor will be able to act on its target on the next
/// meaningful action.
///
/// # Signature
///
/// Takes `from_pos` / `to_pos` / `target_pos` explicitly rather than
/// reading `active.pos`. The scorer calls `intent_score` per step with
/// `active = sim_actor` (pre-step perspective), so reading `active.pos`
/// would work today — but the coupling is implicit and brittle. Explicit
/// positions make the helper self-contained and trivially unit-testable.
///
/// # Reach semantics
///
/// Caller picks `reach` to match the intent:
/// - `FocusTarget`: `active.speed + active.max_attack_range` — "will I be
///   able to hit on my next action window".
/// - `ApplyCC`: `active.speed + cc_reach(active, content)` — same shape
///   but measured against the longest-range CC-capable ability.
///
/// Using just `max_attack_range` (without `speed`) would miss the whole
/// point for melee pursuers: a warrior 2 tiles from the target after a
/// move that cuts 3 tiles of distance is semantically "about to engage",
/// and the signal must reflect that.
///
/// # Score shape
///
/// - `new_dist ≤ reach` → `0.8` — entered threat bubble. Strong but still
///   below a direct Cast (`1.0`), so Cast plans always win when castable.
/// - closing (`delta > 0`) → `0.3 × delta/reach`, capped at `0.3`. Mild
///   positive, can't spoof the viability threshold (`0.5` for
///   FocusTarget/ApplyCC) on its own.
/// - retreat (`delta < 0`) → `-0.1 × |delta|/reach`, capped at `0.1`.
///   Proportional and soft — a temporary step backward around a choke or
///   an obstacle barely registers, position/risk factors handle the rest.
/// - no change → `0.0`.
pub fn pursuit_move_score(from_pos: Hex, to_pos: Hex, target_pos: Hex, reach: u32) -> f32 {
    let new_dist = to_pos.unsigned_distance_to(target_pos);
    if new_dist <= reach {
        return 0.8;
    }
    let reach_f = reach.max(1) as f32;
    let cur_dist = from_pos.unsigned_distance_to(target_pos) as i32;
    let delta = cur_dist - new_dist as i32;
    if delta > 0 {
        (0.3 * delta as f32 / reach_f).min(0.3)
    } else if delta < 0 {
        -(0.1 * ((-delta) as f32 / reach_f).min(1.0))
    } else {
        0.0
    }
}

/// Longest CC-capable range in the actor's kit. Used by `ApplyCC`
/// pursuit scoring to define the "engagement horizon" — a Move that
/// brings the actor within `speed + cc_reach` of the CC target is
/// setting up a next-turn stun, which is the whole point of the intent.
///
/// Falls back to `max_attack_range` when the actor has no CC-tagged
/// ability (e.g. weapon-attached stun via status that doesn't fire
/// `applies_cc`). Conservative default — won't over-promise.
pub fn cc_reach(active: &UnitSnapshot, content: &ContentView) -> u32 {
    active
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| applies_cc(def, content))
        .map(|def| def.range.max)
        .max()
        .unwrap_or(active.max_attack_range)
}

// ── IntentWeights ────────────────────────────────────────────────────────────

/// Per-intent dot-product weight vector over `PlanFactors`.
///
/// Only the fields explicitly set matter; all others default to 0.0. Builder
/// methods mirror the `PlanFactors` field names so the weight declaration reads
/// as a direct mapping: `IntentWeights::default().kill_now(2.0).damage(1.0)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IntentWeights {
    pub damage: f32,
    pub kill_now: f32,
    pub kill_promised: f32,
    pub cc: f32,
}

impl IntentWeights {
    pub fn damage(mut self, w: f32) -> Self { self.damage = w; self }
    pub fn kill_now(mut self, w: f32) -> Self { self.kill_now = w; self }
    pub fn kill_promised(mut self, w: f32) -> Self { self.kill_promised = w; self }
    pub fn cc(mut self, w: f32) -> Self { self.cc = w; self }

    /// Dot product of weights against a `PlanFactors` (only the covered fields).
    pub fn dot(&self, f: &PlanFactors) -> f32 {
        self.damage * f.damage
            + self.kill_now * f.kill_now
            + self.kill_promised * f.kill_promised
            + self.cc * f.cc
    }
}

// ── Per-target offensive filtering ──────────────────────────────────────────

/// Filter the offensive axes of `factors` to only credit actions that
/// directly advance a targeted intent on `focus_entity`.
///
/// Rules (by step type):
/// - `Cast` directly targeting `focus_entity`: full credit (factors unchanged).
/// - `Cast` of an AoE that covers `focus_entity`'s tile: offensive axes
///   (damage, kill_now, kill_promised, cc) scaled by 0.6.
/// - `Cast` not involving `focus_entity`: offensive axes zeroed.
/// - `Move`: offensive axes zeroed — geometry hook handles Move alignment.
fn filter_offensive_for_target(
    mut factors: PlanFactors,
    focus_entity: Entity,
    step: &ScoredStep,
    snap: &BattleSnapshot,
    content: &ContentView,
) -> PlanFactors {
    match step {
        ScoredStep::Move { .. } => {
            // Move steps: no offensive credit; geometry hook drives scoring.
            factors.damage = 0.0;
            factors.kill_now = 0.0;
            factors.kill_promised = 0.0;
            factors.cc = 0.0;
            factors
        }
        ScoredStep::Cast { ability, target, target_pos, caster_tile } => {
            if *target == focus_entity {
                // Direct hit on the focus entity: full credit.
                return factors;
            }
            // Check if an AoE covers the focus entity's tile.
            if let Some(def) = content.abilities.get(*ability) {
                if def.aoe != AoEShape::None {
                    if let Some(focus_unit) = snap.unit(focus_entity) {
                        let area = aoe_area(def, *target_pos, *caster_tile);
                        if area.contains(&focus_unit.pos) {
                            // AoE that includes the focus tile: partial credit.
                            factors.damage *= 0.6;
                            factors.kill_now *= 0.6;
                            factors.kill_promised *= 0.6;
                            factors.cc *= 0.6;
                            return factors;
                        }
                    }
                }
            }
            // No involvement of the focus entity: zero out offensive axes.
            factors.damage = 0.0;
            factors.kill_now = 0.0;
            factors.kill_promised = 0.0;
            factors.cc = 0.0;
            factors
        }
    }
}

// ── Intent → utility score (factor[7]) ──────────────────────────────────────

/// Compute how well a scored step aligns with the current intent.
/// Positive = aligned, zero = neutral, negative = misaligned (soft penalty).
///
/// Uses a dot-product of per-step impact factors against intent-specific weight
/// vectors (via `IntentWeights`) for `FocusTarget` and `ApplyCC`. This makes
/// alignment proportional to actual impact magnitude — a hit doing 10 damage
/// outscores a hit doing 1 damage, fixing S5 (low-value armor hits getting full
/// intent credit under the old hardcoded 1.0 return).
///
/// `ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand` preserve their
/// existing formulas (ported to the new signature).
pub fn intent_score(
    intent: &TacticalIntent,
    step: &ScoredStep,
    step_ctx: &ScoringCtx,
) -> f32 {
    let active = step_ctx.active;
    let snap = step_ctx.snap;
    let maps = step_ctx.maps;
    let content = step_ctx.world.content;
    let difficulty = step_ctx.world.difficulty;
    let mild_penalty = step_ctx.world.tuning.thresholds.mild_penalty;

    // Move steps: scored only on position-related intent axes.
    let cast = match step {
        ScoredStep::Cast { ability, target_pos, target, .. } => {
            Some((*ability, *target_pos, *target))
        }
        ScoredStep::Move { .. } => None,
    };

    match intent {
        TacticalIntent::FocusTarget { target: focus } => {
            if cast.is_none() {
                // Pure move: pursuit geometry hook.
                return match snap.unit(*focus) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(active.max_attack_range);
                        pursuit_move_score(active.pos, step.caster_tile(), t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Cast: compute per-step factors, filter to focus target, dot with weights.
            let raw = compute_factors(step_ctx, step);
            let filtered = filter_offensive_for_target(raw, *focus, step, snap, content);
            let weights = IntentWeights::default()
                .kill_now(2.0)
                .kill_promised(0.3)
                .damage(1.0)
                .cc(0.5);
            weights.dot(&filtered)
        }
        TacticalIntent::ApplyCC { target: cc_target } => {
            if cast.is_none() {
                // Pure move during ApplyCC: reach uses CC-capable range.
                return match snap.unit(*cc_target) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(cc_reach(active, content));
                        pursuit_move_score(active.pos, step.caster_tile(), t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Cast: compute per-step factors, filter to CC target, dot with weights.
            let raw = compute_factors(step_ctx, step);
            let filtered = filter_offensive_for_target(raw, *cc_target, step, snap, content);
            let weights = IntentWeights::default()
                .cc(1.5)
                .damage(0.3);
            weights.dot(&filtered)
        }
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
                    if target == *ally { 1.0 } else { mild_penalty }
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
                return mild_penalty;
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
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::influence::InfluenceMaps;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
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

        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ab = AbilityId::from("melee_attack");

        let ctx_worse = make_scoring_ctx(&world, &snap, &maps, &reservations, &active);
        let score_worse = intent_score(&intent, &dummy_step(worse, &ab), &ctx_worse);

        let ctx_better = make_scoring_ctx(&world, &snap, &maps, &reservations, &active);
        let score_better = intent_score(&intent, &dummy_step(better, &ab), &ctx_better);

        assert!(
            score_worse < 0.0,
            "worse tile should be penalized, got {score_worse}"
        );
        assert!(
            score_better > 0.0,
            "better tile should score positively, got {score_better}"
        );
    }

    // ── pursuit_move_score: pure helper ─────────────────────────────────

    /// Enter-reach gives the strong signal (0.8). Same bonus whether we
    /// land adjacent or at the reach boundary — caller's position/risk
    /// factors differentiate within the bubble.
    #[test]
    fn pursuit_entering_reach_scores_full_bonus() {
        let from = hex_from_offset(0, 0);
        let target = hex_from_offset(6, 0);
        // reach = 4: new tile at dist=4 from target qualifies.
        let landing = hex_from_offset(2, 0); // dist=4 from target
        let score = pursuit_move_score(from, landing, target, 4);
        assert!((score - 0.8).abs() < 1e-5, "enter-reach expected 0.8, got {score}");

        // Also enters when landing adjacent (dist=1 ≤ 4).
        let adj = hex_from_offset(5, 0); // dist=1
        let score_adj = pursuit_move_score(from, adj, target, 4);
        assert!((score_adj - 0.8).abs() < 1e-5);
    }

    /// Closing (outside reach) is mild positive, capped at 0.3 — can't
    /// spoof the 0.5 viability threshold on its own.
    #[test]
    fn pursuit_closing_is_mild_positive() {
        // from dist=10, to dist=7 — delta=3, reach=4, expected 0.3*3/4=0.225
        let from = hex_from_offset(10, 0);
        let to = hex_from_offset(7, 0);
        let target = hex_from_offset(0, 0);
        let score = pursuit_move_score(from, to, target, 4);
        assert!((score - 0.225).abs() < 1e-5, "closing: {score}");
        assert!(score < 0.5, "closing alone must stay below viability threshold");
        assert!(score > 0.0);
    }

    /// Retreat is softly negative and proportional — a single-tile back-
    /// step at reach=4 barely registers, so hex-grid detours around
    /// chokes or obstacles aren't punished.
    #[test]
    fn pursuit_retreat_is_soft_negative() {
        // from dist=5, to dist=6 — delta=-1, reach=4, expected -0.1*1/4=-0.025
        let from = hex_from_offset(5, 0);
        let to = hex_from_offset(6, 0);
        let target = hex_from_offset(0, 0);
        let score = pursuit_move_score(from, to, target, 4);
        assert!((score + 0.025).abs() < 1e-5, "retreat: {score}");
        assert!(score > -0.1, "retreat capped at -0.1, got {score}");
    }

    /// No change in hex distance (e.g. circling around an equidistant
    /// arc on hex-grid) scores 0 — neutral, not punished.
    #[test]
    fn pursuit_no_distance_change_is_zero() {
        // Target far (dist=10), reach=2: any equidistant neighbor stays
        // outside the bubble, so the test exercises the delta==0 branch
        // rather than accidentally tripping the enter-reach early return.
        let from = hex_from_offset(10, 0);
        let target = hex_from_offset(0, 0);
        let cur_d = from.unsigned_distance_to(target);
        let equidistant = from
            .all_neighbors()
            .into_iter()
            .find(|&n| n.unsigned_distance_to(target) == cur_d)
            .expect("even-r hex should admit an equidistant neighbor on a straight axis");
        let score = pursuit_move_score(from, equidistant, target, 2);
        assert_eq!(score, 0.0);
    }

    // ── cc_reach: content-aware reach computation ───────────────────────

    /// Actor has a ranged stun (range=3) and a melee weapon_attack
    /// (range=1). `cc_reach` must pick the stun's range — that's the
    /// intent-relevant engagement horizon.
    #[test]
    fn cc_reach_prefers_cc_ability_range() {
        use crate::content::abilities::{
            AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn,
            TargetType,
        };
        use crate::content::statuses::StatusDef;
        use crate::core::{DiceExpr, StatusId};

        let mut content = crate::combat::ai::test_helpers::empty_content();
        let stun_status_id = StatusId::from("stun");
        content.statuses.insert(
            stun_status_id.clone(),
            StatusDef {
                id: stun_status_id.clone(),
                name: "stun".into(),
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: true,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: false,
                buff_class: None,
            },
        );
        let stun_shot = AbilityDef {
            id: AbilityId::from("stun_shot"),
            name: "stun_shot".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication {
                status: stun_status_id,
                duration_rounds: 1,
                on: StatusOn::Target,
            }],
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };
        let melee = AbilityDef {
            id: AbilityId::from("melee_attack"),
            name: "melee_attack".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };
        content.abilities.insert(stun_shot.id.clone(), stun_shot.clone());
        content.abilities.insert(melee.id.clone(), melee.clone());

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ability_names(&["stun_shot", "melee_attack"])
            .max_attack_range(3)
            .build();
        assert_eq!(cc_reach(&actor, &content), 3);

        // Actor without any CC ability falls back to max_attack_range.
        let brawler = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .ability_names(&["melee_attack"])
            .max_attack_range(1)
            .build();
        assert_eq!(cc_reach(&brawler, &content), 1);
    }

    // ── intent_score wiring: FocusTarget Move uses pursuit ──────────────

    /// Regression test for logs #1/#3/#7: a melee pursuer whose Move
    /// enters the (speed + range) bubble must score at/above the
    /// FocusTarget viability threshold (0.5). Before Fix B Move scored
    /// 0.0, so viability_fallback ran every turn even when the warrior
    /// was actively closing.
    #[test]
    fn focus_target_pursuit_enters_bubble_above_viability() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .speed(3)
            .max_attack_range(1)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0))
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };

        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // Move to (4,0) — dist=1 to target, reach=3+1=4, 1<=4 → 0.8.
        let move_into_reach = ScoredStep::Move { caster_tile: hex_from_offset(4, 0) };
        let score = intent_score(&intent, &move_into_reach, &ctx);
        assert!(
            score >= 0.5,
            "enter-reach Move must pass viability (0.5), got {score}",
        );
    }

    // ── intent_score: FocusTarget proportional scoring ──────────────────

    /// FocusTarget intent score must be proportional to actual damage dealt:
    /// hitting the focus target for 10 damage must outscore hitting it for 1.
    /// This pins the S5 fix — armor hits that do minimal damage no longer
    /// receive the same credit as impactful blows.
    #[test]
    fn focus_target_scores_proportional_to_damage() {
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
        use crate::core::DiceExpr;

        let target_pos = hex_from_offset(1, 0);
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(20).build();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let difficulty = DifficultyProfile::default();

        // Two abilities: one deals 10 damage, the other 1 damage.
        let mut content = crate::combat::ai::test_helpers::empty_content();
        let strong = AbilityDef {
            id: AbilityId::from("strong_hit"),
            name: "strong_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 10, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };
        let weak = AbilityDef {
            id: AbilityId::from("weak_hit"),
            name: "weak_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 1, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };
        content.abilities.insert(strong.id.clone(), strong.clone());
        content.abilities.insert(weak.id.clone(), weak.clone());

        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let strong_id = AbilityId::from("strong_hit");
        let weak_id = AbilityId::from("weak_hit");
        let step_strong = ScoredStep::Cast {
            ability: &strong_id,
            target: target.entity,
            target_pos,
            caster_tile: actor.pos,
        };
        let step_weak = ScoredStep::Cast {
            ability: &weak_id,
            target: target.entity,
            target_pos,
            caster_tile: actor.pos,
        };

        let score_strong = intent_score(&intent, &step_strong, &ctx);
        let score_weak = intent_score(&intent, &step_weak, &ctx);

        assert!(
            score_strong > score_weak,
            "high-damage hit ({score_strong}) must outscore low-damage hit ({score_weak})",
        );
        assert!(score_strong > 0.0, "strong hit must score positively: {score_strong}");
        assert!(score_weak >= 0.0, "weak hit must not score negatively: {score_weak}");
    }

    /// Hitting a non-focus target with a single-target attack should yield
    /// near-zero intent score for FocusTarget intent (no offensive credit for
    /// the focus entity).
    #[test]
    fn focus_target_wrong_target_scores_near_zero() {
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
        use crate::core::DiceExpr;

        let focus_pos = hex_from_offset(1, 0);
        let other_pos = hex_from_offset(2, 0);
        let focus = UnitBuilder::new(2, Team::Player, focus_pos).hp(20).build();
        let other = UnitBuilder::new(3, Team::Player, other_pos).hp(20).build();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone(), other.clone()], 1);
        let maps = empty_maps();
        let difficulty = DifficultyProfile::default();

        let mut content = crate::combat::ai::test_helpers::empty_content();
        let hit = AbilityDef {
            id: AbilityId::from("melee_hit"),
            name: "melee_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(2, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };
        content.abilities.insert(hit.id.clone(), hit);

        let intent = TacticalIntent::FocusTarget { target: focus.entity };
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let ability_id = AbilityId::from("melee_hit");
        // Cast targeting `other` (not the focus entity).
        let step_wrong = ScoredStep::Cast {
            ability: &ability_id,
            target: other.entity,
            target_pos: other_pos,
            caster_tile: actor.pos,
        };

        let score = intent_score(&intent, &step_wrong, &ctx);
        assert!(
            score <= 0.0,
            "hitting non-focus target must yield ≤ 0 intent score, got {score}",
        );
    }

    // ── PlanSnapshot: invalidation detection ────────────────────────────

    use crate::combat::ai::snapshot::ActiveStatusView;
    use crate::core::StatusId;

    fn make_status(id: &str, rounds: u32) -> ActiveStatusView {
        ActiveStatusView { id: StatusId::from(id), rounds_remaining: rounds, dot_per_tick: 0 }
    }

    #[test]
    fn snapshot_matches_unchanged_state() {
        let expected_pos = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, expected_pos).hp(10).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0)).build();
        let _snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);

        let stored = PlanSnapshot::capture(&actor, Some(&target), expected_pos);
        assert_eq!(stored.mismatch(&actor, Some(&target)), None);
    }

    #[test]
    fn snapshot_detects_actor_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor_before = UnitBuilder::new(1, Team::Enemy, pos).hp(10).build();
        let actor_after = UnitBuilder::new(1, Team::Enemy, pos).hp(8).build(); // AoO hit
        let _snap = BattleSnapshot::new(vec![actor_before.clone()], 1);

        let stored = PlanSnapshot::capture(&actor_before, None, pos);
        assert_eq!(stored.mismatch(&actor_after, None), Some("actor_hp_drop"));
    }

    #[test]
    fn snapshot_detects_actor_status_change() {
        let pos = hex_from_offset(0, 0);
        let mut actor_clean = UnitBuilder::new(1, Team::Enemy, pos).build();
        let mut actor_debuffed = actor_clean.clone();
        actor_debuffed.statuses.push(make_status("burn", 2));

        let stored = PlanSnapshot::capture(&actor_clean, None, pos);
        assert_eq!(stored.mismatch(&actor_debuffed, None), Some("actor_status_changed"));

        // Inverse: had status, now expired.
        actor_clean.statuses.push(make_status("burn", 2));
        let stored2 = PlanSnapshot::capture(&actor_clean, None, pos);
        let mut actor_cured = actor_clean.clone();
        actor_cured.statuses.clear();
        assert_eq!(stored2.mismatch(&actor_cured, None), Some("actor_status_changed"));
    }

    #[test]
    fn snapshot_detects_target_death() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).build();

        let stored = PlanSnapshot::capture(&actor, Some(&target), pos);
        // Target gone from snapshot (ally killed it between ticks).
        assert_eq!(stored.mismatch(&actor, None), Some("target_gone"));
    }

    #[test]
    fn snapshot_detects_target_hp_drop() {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let target_before = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).hp(10).build();
        let target_after = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).hp(4).build();

        let stored = PlanSnapshot::capture(&actor, Some(&target_before), pos);
        assert_eq!(stored.mismatch(&actor, Some(&target_after)), Some("target_hp_drop"));
    }

    #[test]
    fn snapshot_detects_actor_pos_mismatch() {
        let expected = hex_from_offset(3, 0);
        let actual = hex_from_offset(2, 0); // AoO truncated the path
        let actor_at_expected = UnitBuilder::new(1, Team::Enemy, expected).build();
        let actor_at_actual = UnitBuilder::new(1, Team::Enemy, actual).build();

        let stored = PlanSnapshot::capture(&actor_at_expected, None, expected);
        // Actor captured at expected pos, but now at actual (path truncated).
        assert_eq!(stored.mismatch(&actor_at_actual, None), Some("actor_pos_mismatch"));
    }

    /// AP=0: enemy is reachable and within threat, but no action budget →
    /// `killable` filter must skip and fall through to `BestPriority`.
    #[test]
    fn killable_requires_action_points() {
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0); // distance=1, within reach_budget=4+1=5

        // ap=0, speed=4, max_attack_range=1, threat=8 > enemy eff_hp=3
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(0)
            .speed(4)
            .max_attack_range(1)
            .threat(8.0)
            .build();

        // enemy: eff_hp=3 (hp=3, armor=0), reachable and killable in threat
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(3)
            .build();

        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let memory = AiMemory::default();
        let difficulty = DifficultyProfile::default();

        let tuning = AiTuning::default();
        let choice = select_intent(&actor, &snap, &maps, &memory, &difficulty, &tuning);

        assert!(
            !matches!(choice.reason, IntentReason::Killable { .. }),
            "AP=0 must not yield Killable; got {:?}",
            choice.reason,
        );
        assert!(
            matches!(choice.reason, IntentReason::BestPriority { .. }),
            "AP=0 should fall through to BestPriority; got {:?}",
            choice.reason,
        );
    }
}
