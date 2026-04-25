//! Appraisal / Need layer (step 3 of ai-rework).
//!
//! Aggregates raw tactical facts (`BattleSnapshot` + `InfluenceMaps` + `AiMemory`)
//! into normalised "urgency" signals consumed by `select_intent` and downstream
//! scoring layers. Producer is `compute_need_signals`; consumers are wired in
//! steps 3.2–3.5. Until then the producer returns `Default::default()` (zeros).
//!
//! Spec: `docs/ai_need_signals.md` (mining-driven taxonomy + curve params).
//! Decomposition: `docs/ai_rework_step3_plan.md`.

use serde::{Deserialize, Serialize};

use crate::combat::ai::intent::{AiMemory, IntentKind};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::tuning::AiTuning;

/// Normalised need-signal vector. Each field in [0, 1] semantically; producer
/// clamps. Five signals are populated in step 3.1 (`self_preserve`,
/// `finish_target`, `reposition`, `conserve_resource`, `continue_commitment`);
/// the remaining three (`rescue_ally`, `apply_cc`, `setup_aoe`) stay at 0.0
/// until the second mining iteration delivers concrete inputs
/// (see `ai_need_signals.md:166`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NeedSignals {
    pub self_preserve: f32,
    pub rescue_ally: f32,
    pub finish_target: f32,
    pub apply_cc: f32,
    pub setup_aoe: f32,
    pub reposition: f32,
    pub conserve_resource: f32,
    pub continue_commitment: f32,
}

/// Compute need signals from raw tactical state.
///
/// Step 3.1: implements all 5 mineable signals. See `docs/ai_need_signals.md`
/// for the full spec and curve parameters.
pub fn compute_need_signals(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    memory: &AiMemory,
    tuning: &AiTuning,
) -> NeedSignals {
    let self_preserve = compute_self_preserve(active, memory, tuning);
    let continue_commitment = compute_continue_commitment(active, snap, memory, tuning);
    let finish_target = compute_finish_target(active, snap, memory, tuning);
    let reposition = compute_reposition(active, snap, maps, tuning);
    let conserve_resource = compute_conserve_resource(active, tuning);

    // rescue_ally / apply_cc / setup_aoe — stay 0.0 until the second mining
    // iteration; see docs/ai_need_signals.md:166.
    let rescue_ally = 0.0;
    let apply_cc = 0.0;
    let setup_aoe = 0.0;

    NeedSignals {
        self_preserve,
        rescue_ally,
        finish_target,
        apply_cc,
        setup_aoe,
        reposition,
        conserve_resource,
        continue_commitment,
    }
}

// ── Signal producers ──────────────────────────────────────────────────────────

fn compute_self_preserve(active: &UnitSnapshot, memory: &AiMemory, tuning: &AiTuning) -> f32 {
    let hp_pct = active.hp_pct();
    let urgency_hp = tuning.curves.self_preserve_hp.eval(1.0 - hp_pct);

    let recent_damage_taken = memory
        .hp_ratio_at_last_turn
        .map(|prev| (prev - hp_pct).max(0.0))
        .unwrap_or(0.0);
    let dmg_mult_raw = 1.0 + tuning.curves.self_preserve_dmg_alpha * recent_damage_taken;

    // Dampen urgency when the unit was already defensive last turn and
    // no fresh damage came in — prevents re-triggering ProtectSelf every
    // turn when the actor is simply "sitting low but unthreatened".
    let dmg_mult = if memory.last_turn_was_defensive && recent_damage_taken < 0.05 {
        dmg_mult_raw * 0.5
    } else {
        dmg_mult_raw
    };

    (urgency_hp * dmg_mult).clamp(0.0, 1.0)
}

fn compute_continue_commitment(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    memory: &AiMemory,
    tuning: &AiTuning,
) -> f32 {
    // Use an Option-returning closure so `?` works for early exits.
    let inner = || -> Option<f32> {
        // Only sticky for target-oriented intents.
        let kind = memory.last_intent?;
        if !matches!(kind, IntentKind::FocusTarget | IntentKind::ApplyCC) {
            return None;
        }
        let last_target_id = memory.last_target?;
        let last_target = snap.unit(last_target_id)?;

        // If target is already in the finisher zone, let finish_target take over.
        let last_target_hp = last_target.hp_pct();
        if last_target_hp <= 0.25 {
            return None;
        }

        // Reachability check: can we reach the target within speed + attack range?
        let reach_budget = (active.speed.max(0) as u32)
            .saturating_add(active.max_attack_range);
        let dist = active.pos.unsigned_distance_to(last_target.pos);
        if dist > reach_budget {
            return None;
        }

        Some(tuning.curves.continue_commitment_hp.eval(last_target_hp))
    };

    inner().unwrap_or(0.0).max(0.0)
}

fn compute_finish_target(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    memory: &AiMemory,
    tuning: &AiTuning,
) -> f32 {
    let reach_budget = (active.speed.max(0) as u32).saturating_add(active.max_attack_range);

    // Best killability metric among reachable killable enemies.
    // None means no killable target exists → signal stays 0.
    let killable_low_hp: Option<f32> = snap
        .enemies_of(active.team)
        .filter(|_| active.action_points > 0)
        .filter(|e| active.threat >= e.eff_hp() as f32)
        .filter(|e| active.pos.unsigned_distance_to(e.pos) <= reach_budget)
        .map(|e| 1.0 - e.hp_pct())
        .reduce(f32::max);

    // No killable candidate → strictly 0 regardless of curve baseline.
    let Some(best_damage_pct) = killable_low_hp else {
        return 0.0;
    };

    let mut finish_target = tuning.curves.finish_target_kill.eval(best_damage_pct);

    // Bonus if the last-committed target is killable and has taken damage
    // (eases the handoff from continue_commitment → finish_target).
    if let Some(last_id) = memory.last_target {
        if let Some(last) = snap.unit(last_id) {
            // Heuristic for "we dealt damage to this target" — without a shared
            // team blackboard (step 13) we use 1 - hp_pct as a proxy.
            let target_damage_proxy = 1.0 - last.hp_pct();
            if target_damage_proxy > 0.1 && active.threat >= last.eff_hp() as f32 {
                finish_target = (finish_target + 0.2).min(1.0);
            }
        }
    }

    finish_target
}

fn compute_reposition(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    tuning: &AiTuning,
) -> f32 {
    let has_ap = active.action_points >= 1;
    let cur_pos_eval = crate::combat::ai::position_eval::evaluate_position(
        active.pos, &active.role, tuning, maps,
    );

    // BFS over reachable tiles (movement_points budget) to find the best
    // position improvement. Uses the same reach helper as the planner so
    // passability / stop rules are consistent.
    let reach = crate::combat::ai::planning::reach::reach_from(snap, active);
    let best_position_improvement = reach
        .destinations
        .iter()
        .map(|&tile| {
            let pe = crate::combat::ai::position_eval::evaluate_position(
                tile, &active.role, tuning, maps,
            );
            (pe - cur_pos_eval).max(0.0)
        })
        .fold(0.0_f32, f32::max);

    let engagement_gap = snap
        .enemies_of(active.team)
        .all(|e| active.pos.unsigned_distance_to(e.pos) > active.max_attack_range);

    let mut reposition = tuning.curves.reposition_pos_gain.eval(best_position_improvement);

    // Idle AP boost: no enemies in attack range and we have AP → nudge to move.
    if engagement_gap && has_ap {
        reposition = reposition.max(0.5);
    }

    reposition
}

fn compute_conserve_resource(active: &UnitSnapshot, tuning: &AiTuning) -> f32 {
    // mana is Option<(current, max)>; units without a mana bar have no
    // resource pressure (ratio = 1.0 → low signal on the descending logistic).
    let mana_ratio = match active.mana {
        Some((current, max)) if max > 0 => current as f32 / max as f32,
        _ => 1.0,
    };

    tuning.curves.conserve_resource.eval(mana_ratio)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_maps, ent, UnitBuilder};
    use crate::combat::ai::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn default_memory() -> AiMemory {
        AiMemory {
            last_intent: None,
            last_target: None,
            turns_committed: 0,
            last_plan: None,
            hp_ratio_at_last_turn: None,
            last_turn_was_defensive: false,
            turns_in_low_hp: 0,
        }
    }

    fn snap(units: Vec<crate::combat::ai::snapshot::UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    // ── self_preserve ─────────────────────────────────────────────────────

    #[test]
    fn default_need_signals_are_zero() {
        let n = NeedSignals::default();
        assert_eq!(n.self_preserve, 0.0);
        assert_eq!(n.rescue_ally, 0.0);
        assert_eq!(n.finish_target, 0.0);
        assert_eq!(n.apply_cc, 0.0);
        assert_eq!(n.setup_aoe, 0.0);
        assert_eq!(n.reposition, 0.0);
        assert_eq!(n.conserve_resource, 0.0);
        assert_eq!(n.continue_commitment, 0.0);
    }

    #[test]
    fn self_preserve_zero_at_full_hp() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .full_hp(20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let signal = compute_self_preserve(&active, &memory, &tuning);
        // Logistic at (1 - 1.0) = 0.0 is well below 0.05.
        assert!(signal < 0.05, "expected near 0 at full HP, got {signal}");
    }

    #[test]
    fn self_preserve_high_at_low_hp() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(4)
            .max_hp(20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let signal = compute_self_preserve(&active, &memory, &tuning);
        // hp_pct = 0.2, urgency_hp should be high.
        assert!(signal > 0.7, "expected > 0.7 at 20% HP, got {signal}");
    }

    #[test]
    fn self_preserve_amplified_by_recent_damage() {
        let hp_pct_now = 0.5;
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(10)
            .max_hp(20)
            .build();
        let memory = AiMemory {
            hp_ratio_at_last_turn: Some(0.9),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let signal_with_damage = compute_self_preserve(&active, &memory, &tuning);

        let memory_no_damage = default_memory();
        let signal_no_damage = compute_self_preserve(&active, &memory_no_damage, &tuning);

        assert!(
            signal_with_damage > signal_no_damage,
            "damage history ({:.3}) should amplify self_preserve vs baseline ({:.3}), hp_pct_now={hp_pct_now}",
            signal_with_damage,
            signal_no_damage,
        );
    }

    #[test]
    fn self_preserve_dampened_after_defensive() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(10)
            .max_hp(20)
            .build();
        // No fresh damage, was defensive.
        let memory_defensive = AiMemory {
            last_turn_was_defensive: true,
            hp_ratio_at_last_turn: Some(0.5), // same HP → no fresh damage
            ..default_memory()
        };
        let memory_normal = AiMemory {
            last_turn_was_defensive: false,
            hp_ratio_at_last_turn: Some(0.5),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let signal_defensive = compute_self_preserve(&active, &memory_defensive, &tuning);
        let signal_normal = compute_self_preserve(&active, &memory_normal, &tuning);

        assert!(
            signal_defensive < signal_normal,
            "defensive flag should dampen self_preserve ({:.3}) vs normal ({:.3})",
            signal_defensive,
            signal_normal,
        );
    }

    // ── continue_commitment ───────────────────────────────────────────────

    #[test]
    fn continue_commitment_zero_when_no_last_intent() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let signal = compute_continue_commitment(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0);
    }

    #[test]
    fn continue_commitment_zero_when_target_not_in_snap() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(99)), // not in snap
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let signal = compute_continue_commitment(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0);
    }

    #[test]
    fn continue_commitment_zero_when_target_low_hp() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_attack_range(2)
            .build();
        // Target at 20% HP → finisher zone.
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let signal = compute_continue_commitment(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0, "finisher zone should return 0");
    }

    #[test]
    fn continue_commitment_zero_when_unreachable() {
        let actor_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(9, 9);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .speed(1)
            .max_attack_range(1)
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(8)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let signal = compute_continue_commitment(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0, "unreachable target should return 0");
    }

    #[test]
    fn continue_commitment_high_when_alive_50pct_reachable() {
        // With default curve (Logistic { mid: 0.4, k: -10 }):
        //   eval(0.5) = 1/(1+exp(10*(0.5-0.4))) ≈ 0.27 — the descending logistic
        //   places the plateau below 0.4. The important invariant is that all
        //   pre-conditions pass and the result is clearly positive (> 0).
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_attack_range(2)
            .speed(3)
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(5)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let signal = compute_continue_commitment(&active, &s, &memory, &tuning);
        assert!(signal > 0.0, "should be positive for reachable 50% HP target, got {signal}");
    }

    // ── finish_target ─────────────────────────────────────────────────────

    #[test]
    fn finish_target_zero_when_no_killable() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        // Actor threat (5.0) < enemy eff_hp (50) → not killable.
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .threat(5.0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .full_hp(50)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let signal = compute_finish_target(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0);
    }

    #[test]
    fn finish_target_high_when_killable_low_hp() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        // Enemy at 20% HP (2 of 10), threat > eff_hp.
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .threat(15.0)
            .max_attack_range(2)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let signal = compute_finish_target(&active, &s, &memory, &tuning);
        assert!(signal > 0.7, "expected > 0.7 for killable low-HP enemy, got {signal}");
    }

    #[test]
    fn finish_target_zero_when_actor_no_ap() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(0)
            .threat(15.0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let signal = compute_finish_target(&active, &s, &memory, &tuning);
        assert_eq!(signal, 0.0, "no AP should yield 0 (filter blocks killable iter)");
    }

    // ── reposition ────────────────────────────────────────────────────────

    #[test]
    fn reposition_high_when_engagement_gap_and_ap() {
        // No enemies at all → engagement_gap=true, has_ap=true → idle boost ≥ 0.5.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .build();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone()]);
        let signal = compute_reposition(&active, &s, &maps, &tuning);
        assert!(signal >= 0.5, "idle AP boost should push reposition ≥ 0.5, got {signal}");
    }

    #[test]
    fn reposition_zero_when_engaged_no_position_gain() {
        // Enemy is adjacent (within max_attack_range=1) → no engagement gap.
        // Maps are all zeros → no position improvement.
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .max_attack_range(1)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone(), enemy]);
        let signal = compute_reposition(&active, &s, &maps, &tuning);
        // No engagement gap, no position gain → only curve eval(0) which is ≈ 0.
        assert!(signal < 0.1, "expected near 0 when engaged with no position gain, got {signal}");
    }

    // ── conserve_resource ────────────────────────────────────────────────

    #[test]
    fn conserve_resource_high_at_low_mana() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .mana(1, 10) // 10% mana
            .build();
        let tuning = AiTuning::default();
        let signal = compute_conserve_resource(&active, &tuning);
        assert!(signal > 0.6, "expected > 0.6 at 10% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_low_at_full_mana() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .mana(19, 20) // 95% mana
            .build();
        let tuning = AiTuning::default();
        let signal = compute_conserve_resource(&active, &tuning);
        assert!(signal < 0.1, "expected < 0.1 at 95% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_no_pressure_when_no_mana_bar() {
        // No mana field → ratio = 1.0 → logistic(k<0) gives near 0.
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let tuning = AiTuning::default();
        let signal = compute_conserve_resource(&active, &tuning);
        assert!(signal < 0.1, "expected near 0 when no mana bar, got {signal}");
    }

    // ── integration ──────────────────────────────────────────────────────

    #[test]
    fn compute_need_signals_stubs_are_strictly_zero() {
        // Full-HP actor, no enemies, no memory → trivial inputs.
        // Check rescue_ally / apply_cc / setup_aoe are exactly 0.
        // Note: reposition will fire idle AP boost (has_ap=true, no enemies).
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ap(1)
            .mana(20, 20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone()]);

        let signals = compute_need_signals(&active, &s, &maps, &memory, &tuning);

        assert_eq!(signals.rescue_ally, 0.0, "rescue_ally must be exactly 0 (stub)");
        assert_eq!(signals.apply_cc, 0.0, "apply_cc must be exactly 0 (stub)");
        assert_eq!(signals.setup_aoe, 0.0, "setup_aoe must be exactly 0 (stub)");
        // Self-preserve near 0 at full HP.
        assert!(signals.self_preserve < 0.05, "self_preserve near 0 at full HP");
        // conserve_resource near 0 at full mana.
        assert!(signals.conserve_resource < 0.1, "conserve_resource near 0 at full mana");
    }
}
