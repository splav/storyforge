//! Appraisal / Need layer (step 3 of ai-rework).
//!
//! Aggregates raw tactical facts (`BattleSnapshot` + `InfluenceMaps` + `AiMemory`)
//! into normalised "urgency" signals consumed by `select_intent` and downstream
//! scoring layers. `compute_need_signals` accepts `&AppraisalCtx` and populates
//! 7 signals: `self_preserve`, `continue_commitment`, `finish_target`,
//! `reposition`, `conserve_resource`, `rescue_ally`, `apply_cc`.
//! `setup_aoe` stays at 0.0 — no Setup mechanic exists in shape (see plan §9.B scope).
//!
//! Spec: `docs/ai_need_signals.md` (mining-driven taxonomy + curve params).
//! Decomposition: `docs/ai_rework_step3_plan.md`.

use serde::{Deserialize, Serialize};

use crate::combat::ai::intent::{AiMemory, IntentKind};
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::tags::{AbilityTag, AbilityTagCache, StatusTag, StatusTagCache};
use crate::combat::ai::config::tuning::AiTuning;
use crate::content::content_view::ContentView;

/// Grouping struct for all read-only inputs to `compute_need_signals`.
///
/// Provides tag-cache access for `rescue_ally` / `apply_cc` producers alongside
/// snapshot, influence maps, memory, tuning, and content view.
pub struct AppraisalCtx<'a> {
    pub active:       &'a UnitSnapshot,
    pub snap:         &'a BattleSnapshot,
    pub maps:         &'a InfluenceMaps,
    pub memory:       &'a AiMemory,
    pub tuning:       &'a AiTuning,
    pub ability_tags: &'a AbilityTagCache,
    pub status_tags:  &'a StatusTagCache,
    pub content:      &'a ContentView,
}

/// Normalised need-signal vector. Each field in [0, 1] semantically; producers
/// clamp. Seven signals are populated via `compute_need_signals`: `self_preserve`,
/// `continue_commitment`, `finish_target`, `reposition`, `conserve_resource`,
/// `rescue_ally`, `apply_cc`. `setup_aoe` stays at 0.0 — no Setup mechanic in shape.
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

/// Compute need signals from raw tactical state via `AppraisalCtx`.
///
/// Populates all 7 active signals; `rescue_ally` and `apply_cc` read tag-caches.
/// `setup_aoe` stays at 0.0 — no Setup mechanic in shape (see inline comment).
pub fn compute_need_signals(ctx: &AppraisalCtx<'_>) -> NeedSignals {
    NeedSignals {
        self_preserve:       compute_self_preserve(ctx),
        continue_commitment: compute_continue_commitment(ctx),
        finish_target:       compute_finish_target(ctx),
        reposition:          compute_reposition(ctx),
        conserve_resource:   compute_conserve_resource(ctx),
        rescue_ally:         compute_rescue_ally(ctx),
        apply_cc:            compute_apply_cc(ctx),
        // Setup mechanic absent from shape — activates when channel/marker
        // effects are introduced. Explicit 0.0 pin (not a stub).
        setup_aoe:           0.0,
    }
}

// ── Signal producers ──────────────────────────────────────────────────────────

fn compute_self_preserve(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
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

fn compute_continue_commitment(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
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

fn compute_finish_target(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
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

fn compute_reposition(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;
    let tuning = ctx.tuning;
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

    // Idle AP boost: no enemies in attack range, we have AP, AND there is a
    // real positional improvement to take. Without the improvement gate, the
    // boost forced reposition to fire even when no useful tile existed —
    // post-step-3 mining (3.6) showed this drove Reposition to 15% chosen
    // intent (target 3–5%) and inflated viability_fallback (5.1% → 16.8%)
    // because intent fired without a viable Move plan to back it. Tying the
    // boost to `best_position_improvement >= reposition_pos_gain.x_lo` keeps
    // the idle nudge but only when the curve already says there is somewhere
    // worth going.
    if engagement_gap && has_ap && best_position_improvement >= 0.05 {
        reposition = reposition.max(0.5);
    }

    reposition
}

fn compute_conserve_resource(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let tuning = ctx.tuning;
    // mana is Option<(current, max)>; units without a mana bar have no
    // resource pressure (ratio = 1.0 → low signal on the descending logistic).
    let mana_ratio = match active.mana {
        Some((current, max)) if max > 0 => current as f32 / max as f32,
        _ => 1.0,
    };

    tuning.curves.conserve_resource.eval(mana_ratio)
}

// ── Step 9.B producers ────────────────────────────────────────────────────────

fn compute_rescue_ally(ctx: &AppraisalCtx<'_>) -> f32 {
    // Gate: actor has any ability with Rescue tag in effective kit.
    // `ctx.content.abilities.get(id)` existence check ensures the ability is
    // a known content entry; tag lookup is cache-only (no def access needed).
    let has_rescue_kit = ctx.active.abilities.iter().any(|id| {
        ctx.content.abilities.contains_key(id)
            && ctx.ability_tags.effective(id).contains_tag(AbilityTag::Rescue)
    });
    if !has_rescue_kit {
        return 0.0;
    }

    // Find most-endangered ally within reach budget.
    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.max_attack_range);
    let best_danger: f32 = ctx.snap.units.iter()
        .filter(|a| a.team == ctx.active.team && a.entity != ctx.active.entity)
        .filter(|a| ctx.active.pos.unsigned_distance_to(a.pos) <= reach)
        .map(|a| {
            let hp_low = (1.0 - a.hp_pct()).clamp(0.0, 1.0);
            let threat_to_ally = ally_threat_proxy(a, ctx.snap);
            hp_low * threat_to_ally
        })
        .fold(0.0_f32, f32::max);

    ctx.tuning.curves.rescue_ally.eval(best_danger)
}

/// Estimate the threat level to `ally` from nearby enemies: max DPR among
/// enemies in attack range of the ally, normalised to ≈ [0, 1] by dividing by
/// 10 (mid-game DPR ceiling). Reuses `scoring::horizon_avg` for consistency
/// with the scoring layer.
pub(crate) fn ally_threat_proxy(ally: &UnitSnapshot, snap: &BattleSnapshot) -> f32 {
    snap.units.iter()
        .filter(|e| e.team != ally.team)
        .filter(|e| e.pos.unsigned_distance_to(ally.pos) <= e.max_attack_range)
        .map(crate::combat::ai::scoring::horizon_avg)
        .fold(0.0_f32, f32::max)
        / 10.0
}

fn compute_apply_cc(ctx: &AppraisalCtx<'_>) -> f32 {
    // Gate: actor has any ability with ApplyCC tag in effective kit.
    let has_cc_kit = ctx.active.abilities.iter().any(|id| {
        ctx.content.abilities.contains_key(id)
            && ctx.ability_tags.effective(id).contains_tag(AbilityTag::ApplyCC)
    });
    if !has_cc_kit {
        return 0.0;
    }

    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.max_attack_range);
    let best_threat: f32 = ctx.snap.units.iter()
        .filter(|e| e.team != ctx.active.team)
        .filter(|e| ctx.active.pos.unsigned_distance_to(e.pos) <= reach)
        .filter(|e| !target_already_hardcc(e, ctx.status_tags))
        .map(crate::combat::ai::scoring::horizon_avg)
        .fold(0.0_f32, f32::max);

    // LinearClamped — explicit DPR bounds [2, 10]; more robust than magic /10.
    ctx.tuning.curves.apply_cc.eval(best_threat)
}

/// Returns true if the unit already has a HardCC status applied.
fn target_already_hardcc(unit: &UnitSnapshot, cache: &StatusTagCache) -> bool {
    unit.statuses.iter().any(|st| cache.get(&st.id).contains_tag(StatusTag::HardCC))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, ent, UnitBuilder};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn default_memory() -> AiMemory {
        AiMemory {
            last_intent: None,
            last_target: None,
            turns_committed: 0,
            last_goal: None,
            hp_ratio_at_last_turn: None,
            last_turn_was_defensive: false,
            turns_in_low_hp: 0,
        }
    }

    fn snap(units: Vec<crate::combat::ai::world::snapshot::UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    /// Convenience helper: build an `AppraisalCtx` for unit tests that call
    /// individual producer functions. Uses empty caches and empty maps by default.
    #[allow(clippy::too_many_arguments)]
    fn make_ctx<'a>(
        active: &'a UnitSnapshot,
        battle_snap: &'a BattleSnapshot,
        memory: &'a AiMemory,
        tuning: &'a AiTuning,
        maps: &'a crate::combat::ai::world::influence::InfluenceMaps,
        content: &'a crate::content::content_view::ContentView,
        ability_tags: &'a AbilityTagCache,
        status_tags: &'a StatusTagCache,
    ) -> AppraisalCtx<'a> {
        AppraisalCtx { active, snap: battle_snap, maps, memory, tuning, ability_tags, status_tags, content }
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
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_self_preserve(&ctx);
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
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_self_preserve(&ctx);
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
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();

        let memory_dmg = AiMemory { hp_ratio_at_last_turn: Some(0.9), ..default_memory() };
        let ctx_dmg = make_ctx(&active, &s, &memory_dmg, &tuning, &maps, &content, &at, &st);
        let signal_with_damage = compute_self_preserve(&ctx_dmg);

        let memory_no_damage = default_memory();
        let ctx_no = make_ctx(&active, &s, &memory_no_damage, &tuning, &maps, &content, &at, &st);
        let signal_no_damage = compute_self_preserve(&ctx_no);

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
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();

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
        let ctx_def = make_ctx(&active, &s, &memory_defensive, &tuning, &maps, &content, &at, &st);
        let ctx_nor = make_ctx(&active, &s, &memory_normal, &tuning, &maps, &content, &at, &st);
        let signal_defensive = compute_self_preserve(&ctx_def);
        let signal_normal = compute_self_preserve(&ctx_nor);

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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0);
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0);
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0, "finisher zone should return 0");
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0, "unreachable target should return 0");
    }

    #[test]
    fn continue_commitment_high_when_alive_50pct_reachable() {
        // With default curve (Logistic { mid: 0.4, k: 10 }):
        //   eval(0.5) = 1/(1+exp(-10*(0.5-0.4))) ≈ 0.73.
        // Ascending logistic: high while target is healthy, drops near finisher zone.
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_continue_commitment(&ctx);
        assert!(signal > 0.6, "should be > 0.6 for reachable 50% HP target, got {signal}");
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_finish_target(&ctx), 0.0);
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_finish_target(&ctx);
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
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_finish_target(&ctx), 0.0, "no AP should yield 0 (filter blocks killable iter)");
    }

    // ── reposition ────────────────────────────────────────────────────────

    #[test]
    fn reposition_high_when_engagement_gap_with_real_improvement() {
        // No enemies + has AP + a reachable tile with meaningful pos_eval gain
        // → idle boost ≥ 0.5. Map is built so a neighbouring tile reads as
        // strictly better via the opportunity influence channel (Tank role
        // weights opportunity at +0.9 — see tuning.tables.axis_position_weights).
        let actor_pos = hex_from_offset(3, 3);
        let better_tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .build();
        let tuning = AiTuning::default();
        let mut maps = empty_maps();
        maps.opportunity.add(better_tile, 1.0);
        let s = snap(vec![active.clone()]);
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
        assert!(signal >= 0.5, "idle AP boost should push reposition ≥ 0.5, got {signal}");
    }

    #[test]
    fn reposition_no_boost_when_engagement_gap_but_no_improvement() {
        // No enemies + has AP but flat map (no tile is better than current).
        // Idle boost is gated on real best_position_improvement, so signal
        // collapses to curve.eval(0) ≈ 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .build();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone()]);
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
        assert!(signal < 0.1, "no improvement → no boost, got {signal}");
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
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
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
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(signal > 0.6, "expected > 0.6 at 10% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_low_at_full_mana() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .mana(19, 20) // 95% mana
            .build();
        let tuning = AiTuning::default();
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(signal < 0.1, "expected < 0.1 at 95% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_no_pressure_when_no_mana_bar() {
        // No mana field → ratio = 1.0 → logistic(k<0) gives near 0.
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let tuning = AiTuning::default();
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(signal < 0.1, "expected near 0 when no mana bar, got {signal}");
    }

    // ── integration / setup_aoe pin ───────────────────────────────────────

    #[test]
    fn compute_need_signals_setup_aoe_remains_zero_after_9b() {
        // Explicit pin: setup_aoe stays 0.0 regardless of kit/aoe-shape.
        // No Setup mechanic in shape — see plan §9.B scope.
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
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signals = compute_need_signals(&ctx);
        assert_eq!(signals.setup_aoe, 0.0, "setup_aoe must remain 0.0 — no Setup mechanic in shape");
        assert!(signals.self_preserve < 0.05, "self_preserve near 0 at full HP");
        assert!(signals.conserve_resource < 0.1, "conserve_resource near 0 at full mana");
    }

    // ── rescue_ally ───────────────────────────────────────────────────────

    /// Minimal `AbilityDef` for tests — only sets the override, everything else default.
    fn minimal_ability_def_with_override(tags: &[&str]) -> crate::content::abilities::AbilityDef {
        use crate::content::abilities::{AbilityDef, AbilityRange, EffectDef, TargetType, AoEShape};
        AbilityDef {
            id: "test".into(),
            name: "test".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange::MELEE,
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            magic_domains: vec![],
            magic_method: String::new(),
            key: None,
            ai_tags_override: Some(tags.iter().map(|s| s.to_string()).collect()),
        }
    }

    /// Build a content + ability_tag_cache with a "heal" ability tagged Rescue.
    fn content_with_rescue_ability() -> (crate::content::content_view::ContentView, AbilityTagCache, StatusTagCache) {
        use crate::combat::ai::world::tags::cache::build_caches;

        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&["rescue"]);
        def.id = "heal".into();
        content.abilities.insert("heal".into(), def);
        let (st, at) = build_caches(&content);
        (content, at, st)
    }

    /// Build content + cache with a "stun" ability tagged ApplyCC.
    fn content_with_apply_cc_ability() -> (crate::content::content_view::ContentView, AbilityTagCache, StatusTagCache) {
        use crate::combat::ai::world::tags::cache::build_caches;

        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&["apply_cc"]);
        def.id = "stun".into();
        content.abilities.insert("stun".into(), def);
        let (st, at) = build_caches(&content);
        (content, at, st)
    }

    #[test]
    fn rescue_ally_zero_when_no_rescue_kit() {
        // Actor has no abilities → no Rescue tag → signal = 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos).full_hp(20).build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .hp(4).max_hp(20).build(); // 20% HP — in danger
        let enemy = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 3))
            .threat(8.0).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_rescue_ally(&ctx), 0.0, "no Rescue kit → signal must be 0");
    }

    #[test]
    fn rescue_ally_zero_when_no_allies_in_danger() {
        // Actor has Rescue kit, but ally is at full HP and no enemies threatening.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .full_hp(20).build(); // full HP — not in danger
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally]);
        let maps = empty_maps();
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal < 0.05, "full-HP ally → signal near 0, got {signal}");
    }

    #[test]
    fn rescue_ally_high_when_ally_low_hp_threatened() {
        // Actor has Rescue kit; ally is at 20% HP and an enemy is adjacent.
        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let enemy_pos = hex_from_offset(4, 3); // same tile — adjacent to ally
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build(); // 20% HP
        // Enemy adjacent to ally with high DPR.
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos)
            .threat(8.0)
            .damage_horizon(vec![8.0])
            .max_attack_range(1)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal > 0.6, "low HP ally + high-DPR adjacent enemy → signal > 0.6, got {signal}");
    }

    #[test]
    fn rescue_ally_uses_override_for_kit_check() {
        // Ability with override ["rescue"] — must pass Rescue gate even though
        // underlying EffectDef is not a heal.
        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        // Ally at low HP so signal is non-zero when gate passes.
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build();
        let enemy = UnitBuilder::new(3, Team::Player, ally_pos)
            .threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        // content_with_rescue_ability uses ai_tags_override → must work.
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal > 0.0, "override rescue tag → gate passes, signal > 0, got {signal}");
    }

    #[test]
    fn rescue_ally_zero_when_override_empties_kit() {
        // Ability with override Some([]) → replace-not-append semantics → no tags → gate fails.
        use crate::combat::ai::world::tags::cache::build_caches;

        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build();
        let enemy = UnitBuilder::new(3, Team::Player, ally_pos)
            .threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();

        // "heal" ability with empty override → tags = AbilityTagSet::empty().
        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&[]); // empty override → no tags
        def.id = "heal".into();
        content.abilities.insert("heal".into(), def);
        let (st, at) = build_caches(&content);
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_rescue_ally(&ctx), 0.0, "empty override → gate fails → signal = 0");
    }

    // ── apply_cc ──────────────────────────────────────────────────────────

    #[test]
    fn apply_cc_zero_when_no_cc_kit() {
        // Actor without stun-like ability → no ApplyCC tag → signal = 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos).full_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .full_hp(20).threat(5.0).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_apply_cc(&ctx), 0.0, "no ApplyCC kit → signal must be 0");
    }

    #[test]
    fn apply_cc_zero_when_target_already_hardcc() {
        // Stun in kit, but the only enemy already has HardCC status → gate filters it → 0.
        use crate::combat::ai::world::tags::cache::build_caches;
        use crate::content::statuses::StatusDef;

        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(2)
            .build();

        // Build content with stun ability (ApplyCC) and a "stunned" status (HardCC).
        let mut content = empty_content();
        let mut ability_def = minimal_ability_def_with_override(&["apply_cc"]);
        ability_def.id = "stun".into();
        content.abilities.insert("stun".into(), ability_def);

        let status_def = StatusDef {
            id: "stunned".into(),
            name: "Stunned".into(),
            skips_turn: true, // HardCC — derive_status_tags: skips_turn → HardCC
            armor_bonus: 0,
            damage_taken_bonus: 0,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
            buff_class: None,
        };
        content.statuses.insert("stunned".into(), status_def);

        let (st, at) = build_caches(&content);

        // Enemy already has the stunned status applied.
        let mut enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20).threat(8.0).damage_horizon(vec![8.0]).build();
        enemy.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: "stunned".into(),
            rounds_remaining: 1,
            dot_per_tick: 0,
        });

        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_apply_cc(&ctx);
        assert!(signal < 0.05, "enemy already HardCC → filtered → signal near 0, got {signal}");
    }

    #[test]
    fn apply_cc_high_when_unstunned_threat_in_reach() {
        // Actor has ApplyCC kit; enemy is a high-DPR unstunned threat in reach.
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(2)
            .speed(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20).threat(9.0).damage_horizon(vec![9.0]).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_apply_cc_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_apply_cc(&ctx);
        assert!(signal > 0.5, "unstunned high-DPR enemy in reach → signal > 0.5, got {signal}");
    }

    #[test]
    fn apply_cc_zero_when_no_enemies_in_reach() {
        // Actor has ApplyCC kit but enemies are too far away.
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(9, 9);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(1)
            .speed(1)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20).threat(9.0).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_apply_cc_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_apply_cc(&ctx), 0.0, "enemies out of reach → signal = 0");
    }
}
