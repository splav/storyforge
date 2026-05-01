use bevy::prelude::Entity;
use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::config::tuning::AiTuning;
use crate::combat::ai::factors::ScoredStep;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::scoring::target_priority::{highest_priority_enemy, target_priority};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::game::hex::Hex;
use super::kinds::{IntentKind, IntentReason, TacticalIntent};
use super::memory::AiMemory;

// ── Intent selection result ─────────────────────────────────────────────────

/// Result of intent selection. `reason` captures the actual numbers that made
/// this rule fire — built inline at decision time so a future threshold tweak
/// in `difficulty.rs` can't desync from the logged explanation.
pub struct IntentChoice {
    pub intent: TacticalIntent,
    pub reason: IntentReason,
}

// ── select_intent_normal ────────────────────────────────────────────────────

/// Normal-tactical intent selection: FocusTarget / ApplyCC / SetupAOE / Reposition only.
///
/// This is the inner core of `select_intent` with the hard-override branches removed:
/// - No PanicOverride (handled by `CriticalSelfPreservation` band).
/// - No ProtectSelf urgency-gate (handled by band builders for CSP/HardRescue).
/// - No ProtectAlly healer branch (handled by `HardRescueOpportunity` band).
/// - No Taunt branch (handled by `ForcedTargeting` band).
///
/// Stickiness (continuation bonus) is preserved — it affects which FocusTarget or
/// ApplyCC candidate wins within the normal tactical space.
///
/// Called by `build_normal_tactical` when building the `NormalTactical` band agenda.
pub(crate) fn select_intent_normal(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    memory: &AiMemory,
    difficulty: &DifficultyProfile,
    tuning: &AiTuning,
    need_signals: &NeedSignals,
) -> IntentChoice {
    let _ = (maps, difficulty); // used by outer select_intent; kept for signature symmetry
    let t = &tuning.thresholds;
    let mut best_score = f32::NEG_INFINITY;
    let mut best: Option<IntentChoice> = None;

    let mut consider = |intent: TacticalIntent, score: f32, reason: IntentReason| {
        let mut s = score;
        // Stickiness: same logic as select_intent — bonus for continuing the prior intent.
        if memory.turns_committed < t.max_committed_turns
            && memory.last_intent == Some(intent.kind())
        {
            let stickiness_factor = match intent.kind() {
                IntentKind::FocusTarget | IntentKind::ApplyCC => {
                    need_signals.continue_commitment
                }
                _ => 1.0,
            };
            s += t.stickiness_bonus * stickiness_factor;
            if let (Some(prev), Some(cur)) = (memory.last_target, intent.target()) {
                if prev == cur {
                    s += t.target_stickiness_bonus * stickiness_factor;
                }
            }
        }
        // conserve_resource soft bonus for cheap intents (same as select_intent).
        if need_signals.conserve_resource > t.conserve_resource_threshold {
            let cheap = matches!(
                intent.kind(),
                IntentKind::ProtectSelf | IntentKind::Reposition
            );
            if cheap {
                s += t.conserve_resource_bonus * need_signals.conserve_resource;
            }
        }

        if s > best_score {
            best_score = s;
            best = Some(IntentChoice { intent, reason });
        }
    };

    // NOTE: no taunter check — taunt is handled by ForcedTargeting band.
    // NOTE: no PanicOverride early-return — handled by CriticalSelfPreservation band.
    // NOTE: no ProtectSelf / ProtectAlly — handled by band builders.

    // FocusTarget killable enemy / best priority target.
    let reach_budget = (active.speed.max(0) as u32).saturating_add(active.max_attack_range);
    let killable = snap
        .enemies_of(active.team)
        .filter(|_| active.action_points > 0)
        .filter(|e| active.threat >= e.eff_hp() as f32)
        .filter(|e| active.pos.unsigned_distance_to(e.pos) <= reach_budget)
        .min_by_key(|e| e.eff_hp());
    if let Some(target) = killable {
        let kill_score = 1.2 + need_signals.finish_target * 0.3;
        consider(
            TacticalIntent::FocusTarget { target: target.entity },
            kill_score,
            IntentReason::Killable {
                threat: active.threat,
                eff_hp: target.eff_hp(),
                reach_budget,
                finish_target: need_signals.finish_target,
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

    // Reposition: driven by need_signals.reposition.
    let repo_floor = t.reposition_signal_floor;
    if need_signals.reposition > repo_floor {
        let repo_score = 0.3 + need_signals.reposition * 0.7;
        consider(
            TacticalIntent::Reposition,
            repo_score,
            IntentReason::Reposition {
                reposition: need_signals.reposition,
                floor: repo_floor,
            },
        );
    }

    best.unwrap_or(IntentChoice {
        intent: TacticalIntent::Reposition,
        reason: IntentReason::NoRuleDefault,
    })
}

// ── select_intent (deprecated) ──────────────────────────────────────────────

/// Analyze the battlefield, score all valid intents, and pick the best.
/// Applies stickiness bonus if the previous intent is still reasonable.
///
/// # Deprecation
///
/// **Step 11.5**: This function is deprecated in favour of the band/agenda flow:
/// `assign_band` → `build_agenda` → per-item scoring in `pick_action`.
/// - Normal-tactical routing uses `select_intent_normal` (called from `build_normal_tactical`).
/// - Panic/taunt/rescue routing is handled by `assign_band` + respective band builders.
///
/// Remaining callers: unit tests in this module (testing internal scoring properties).
/// Will be removed in step 12.
#[deprecated(note = "use assign_band → build_agenda flow; direct callers should migrate to \
                     select_intent_normal for NormalTactical. Removal in step 12.")]
pub fn select_intent(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    memory: &AiMemory,
    difficulty: &DifficultyProfile,
    tuning: &AiTuning,
    need_signals: &NeedSignals,
) -> IntentChoice {
    let t = &tuning.thresholds;
    let mut best_score = f32::NEG_INFINITY;
    let mut best: Option<IntentChoice> = None;

    let mut consider = |intent: TacticalIntent, score: f32, reason: IntentReason| {
        let mut s = score;
        // Stickiness: bonus for continuing the same intent, modulated by
        // need_signals.continue_commitment for target-oriented intents.
        // When the prior target is alive, healthy, and reachable,
        // continue_commitment is high (~0.7+) and stickiness works near full.
        // When the target is dead/unreachable/in finisher zone (hp ≤ 0.25),
        // commitment ≈ 0 and stickiness collapses — the AI can freely switch
        // without the flat abandon-penalty noise (mining P1).
        //
        // Non-target intents (ProtectSelf, ProtectAlly, SetupAOE, LastStand)
        // use a flat factor of 1.0 — their stickiness is unrelated to target
        // commitment and should behave as before (step 3.3, variant c).
        if memory.turns_committed < t.max_committed_turns
            && memory.last_intent == Some(intent.kind())
        {
            let stickiness_factor = match intent.kind() {
                IntentKind::FocusTarget | IntentKind::ApplyCC => {
                    need_signals.continue_commitment
                }
                _ => 1.0,
            };
            s += t.stickiness_bonus * stickiness_factor;
            if let (Some(prev), Some(cur)) = (memory.last_target, intent.target()) {
                if prev == cur {
                    s += t.target_stickiness_bonus * stickiness_factor;
                }
            }
        }
        // Step 3.5b: conserve_resource soft bonus for cheap intents.
        // "Cheap" = ProtectSelf and Reposition (AP-only or pure movement, no mana cost).
        // FocusTarget/ApplyCC/SetupAOE/ProtectAlly may involve expensive casts and
        // are not boosted here. Hard budget-aware factor scoring is deferred to step 11
        // (priority bands + scorecard).
        if need_signals.conserve_resource > t.conserve_resource_threshold {
            let cheap = matches!(
                intent.kind(),
                IntentKind::ProtectSelf | IntentKind::Reposition
            );
            if cheap {
                s += t.conserve_resource_bonus * need_signals.conserve_resource;
            }
        }

        if s > best_score {
            best_score = s;
            best = Some(IntentChoice { intent, reason });
        }
    };

    let danger = maps.danger.get(active.pos);

    // Hard override: critically wounded in high danger — survival is non-negotiable.
    // Step 3.2: uses need_signals.self_preserve instead of raw hp_pct.
    // Danger gate still scales with awareness (DifficultyProfile); the HP side
    // now comes from the appraisal layer (logistic curve + recent damage).
    let danger_panic = difficulty.awareness_danger_threshold(tuning);
    let panic_threshold = t.panic_self_preserve_threshold;
    if need_signals.self_preserve >= panic_threshold && danger > danger_panic {
        return IntentChoice {
            intent: TacticalIntent::ProtectSelf,
            reason: IntentReason::PanicOverride {
                self_preserve: need_signals.self_preserve,
                self_preserve_threshold: panic_threshold,
                danger,
                danger_threshold: danger_panic,
            },
        };
    }

    // ProtectSelf: score scales with urgency.
    // Step 3.2: gate and urgency weight use need_signals.self_preserve instead
    // of raw hp_pct. The soft threshold is tunable via Thresholds.
    if need_signals.self_preserve > t.soft_self_preserve_threshold && danger > 0.0 {
        let urgency = need_signals.self_preserve * danger;
        consider(
            TacticalIntent::ProtectSelf,
            urgency,
            IntentReason::Urgency { self_preserve: need_signals.self_preserve, danger },
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
            // Step 3.5a: kill_score uses need_signals.finish_target (global max
            // killability among all reachable killable enemies) instead of the
            // per-target raw (1.0 - hp_pct). The producer filters the same set
            // (action_points > 0, threat >= eff_hp, dist <= reach_budget), so
            // finish_target reflects the best killable opportunity overall.
            let kill_score = 1.2 + need_signals.finish_target * 0.3;
            consider(
                TacticalIntent::FocusTarget { target: target.entity },
                kill_score,
                IntentReason::Killable {
                    threat: active.threat,
                    eff_hp: target.eff_hp(),
                    reach_budget,
                    finish_target: need_signals.finish_target,
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

    // Reposition: drive intent score from need_signals.reposition (computed by
    // the appraisal layer from best_position_improvement, engagement_gap,
    // has_ap). Old gate `pos_eval < awareness_reposition_threshold` was a rough
    // proxy for these inputs — now consolidated in compute_need_signals (3.1
    // producer). Step 3.4 consumer.
    let repo_floor = t.reposition_signal_floor;
    if need_signals.reposition > repo_floor {
        let repo_score = 0.3 + need_signals.reposition * 0.7;
        consider(
            TacticalIntent::Reposition,
            repo_score,
            IntentReason::Reposition {
                reposition: need_signals.reposition,
                floor: repo_floor,
            },
        );
    }

    best.unwrap_or(IntentChoice {
        intent: TacticalIntent::Reposition,
        reason: IntentReason::NoRuleDefault,
    })
}

// ── Viability threshold ─────────────────────────────────────────────────────

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

// ── default_focus_target ────────────────────────────────────────────────────

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

// ── update_memory ───────────────────────────────────────────────────────────

/// Update memory after intent is selected.
///
/// Step 3.0: also tracks `hp_ratio_at_last_turn`, `last_turn_was_defensive`,
/// and `turns_in_low_hp` — inputs for the appraisal / need layer (step 3.1).
pub fn update_memory(
    memory: &mut AiMemory,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    tuning: &AiTuning,
) {
    let kind = intent.kind();
    let target = intent.target();
    if memory.last_intent == Some(kind) && memory.last_target == target {
        memory.turns_committed = memory.turns_committed.saturating_add(1);
    } else {
        memory.turns_committed = 0;
    }
    memory.last_intent = Some(kind);
    memory.last_target = target;

    // Step 3.0: track inputs for need layer (read in step 3.1 producer).
    let hp_pct = active.hp_pct();
    memory.hp_ratio_at_last_turn = Some(hp_pct);
    memory.last_turn_was_defensive = matches!(
        kind,
        IntentKind::ProtectSelf | IntentKind::LastStand
    );
    if hp_pct < tuning.thresholds.low_hp_zone_threshold {
        memory.turns_in_low_hp = memory.turns_in_low_hp.saturating_add(1);
    } else {
        memory.turns_in_low_hp = 0;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::NeedSignals;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

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
        let need_signals = NeedSignals::default();
        let choice = select_intent(&actor, &snap, &maps, &memory, &difficulty, &tuning, &need_signals);

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

    /// Step 3.3: `continue_commitment` modulates FocusTarget stickiness.
    ///
    /// Setup: actor with last_intent=FocusTarget, last_target=E1 (dead — not in
    /// snapshot).  E2 is the only live enemy (BestPriority, score ≈ 0.65).
    /// danger > 0 and self_preserve is high enough that ProtectSelf urgency
    /// (≈ 0.80) slightly beats the raw FocusTarget score.
    ///
    /// - `continue_commitment = 1.0`: stickiness bonus (+0.25) tips FocusTarget
    ///   above ProtectSelf → AI keeps attacking.
    /// - `continue_commitment = 0.0`: no bonus → ProtectSelf wins → AI retreats.
    #[test]
    fn stickiness_modulated_by_continue_commitment() {
        let actor_pos = hex_from_offset(0, 0);
        // E2 is the only live enemy, at moderate distance.
        let e2_pos = hex_from_offset(3, 0); // dist=3, not immediately killable

        // actor: ap=1, speed=1, threat=2 (cannot kill e2 which has eff_hp=10),
        // max_attack_range=1 so reach_budget=2 < dist=3 → not killable.
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(1)
            .max_attack_range(1)
            .threat(2.0)
            .build();

        // E2: full HP 10, so eff_hp=10 > actor threat=2 → not killable.
        let e2 = UnitBuilder::new(2, Team::Player, e2_pos).full_hp(10).build();
        let e2_entity = e2.entity;

        let snap = BattleSnapshot::new(vec![actor.clone(), e2], 1);

        // Danger on actor tile so ProtectSelf urgency = self_preserve * danger.
        let mut maps = empty_maps();
        maps.danger.add(actor_pos, 1.0);

        // Memory: was attacking E1 (now dead — absent from snapshot).
        let dead_entity = bevy::prelude::Entity::from_raw_u32(999).expect("valid");
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(dead_entity),
            turns_committed: 0,
            ..Default::default()
        };

        let difficulty = DifficultyProfile::default();
        let tuning = AiTuning::default();
        // stickiness_bonus = 0.25 (default); soft_self_preserve_threshold = 0.2

        // With commitment = 1.0: FocusTarget stickiness bonus fully applied.
        // FocusTarget BestPriority score ≈ 0.65, + 0.25 stickiness = 0.90
        // ProtectSelf urgency = 0.80 * 1.0 = 0.80   → FocusTarget wins.
        let ns_high = NeedSignals {
            continue_commitment: 1.0,
            self_preserve: 0.80, // above soft threshold (0.2)
            ..NeedSignals::default()
        };
        let choice_high =
            select_intent(&actor, &snap, &maps, &memory, &difficulty, &tuning, &ns_high);
        assert!(
            matches!(choice_high.intent, TacticalIntent::FocusTarget { target } if target == e2_entity),
            "commitment=1.0 → stickiness tips FocusTarget above ProtectSelf; got {:?}",
            choice_high.intent,
        );

        // With commitment = 0.0: stickiness collapses for FocusTarget.
        // FocusTarget score ≈ 0.65 < ProtectSelf urgency 0.80 → ProtectSelf wins.
        let ns_low = NeedSignals {
            continue_commitment: 0.0,
            self_preserve: 0.80,
            ..NeedSignals::default()
        };
        let choice_low =
            select_intent(&actor, &snap, &maps, &memory, &difficulty, &tuning, &ns_low);
        assert!(
            matches!(choice_low.intent, TacticalIntent::ProtectSelf),
            "commitment=0.0 → no stickiness, ProtectSelf beats FocusTarget; got {:?}",
            choice_low.intent,
        );
    }
}
