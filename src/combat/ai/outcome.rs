//! ActionOutcomeEstimate — structured outcome vector shared across factors,
//! intent, critics, and terminal eval. Populated in SimState::apply_step
//! call chain; consumers migrate onto it incrementally (steps 4.1–4.5).
//!
//! Step 4.0 ships the type + PlanAnnotation container zero-filled — no
//! consumers yet. See docs/ai_rework_step4_plan.md.

use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::types::PlanStep;
use crate::combat::ai::scoring::{score_action, status_applications, stun_denial_value};
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::content::abilities::{AbilityDef, CasterContext, TargetType};
use crate::content::content_view::ContentView;
use crate::content::races::CritFailEffect;
use serde::{Deserialize, Serialize};

/// Structured estimate of a single plan step's consequences.
/// Fields populated incrementally across steps 4.1–4.2.
///
/// Semantic note (docs/ai_rework.md §4):
/// - `expected_damage`: raw expected damage from this step (step 4.1, populated).
/// - `p_kill_now`: 1.0 if step kills a target in this turn, else 0.0.
/// - `p_kill_soon`: probability of killing a target within the damage horizon.
/// - `deny_value`: aggregated CC / armor-debuff / vuln "denial" value.
/// - `rescue_value`: heal value with urgency baked-in during wave 1;
///   step 3 (need layer) will split urgency into NeedSignals.rescue_ally.
/// - `board_pressure`: 0.0 placeholder, filled in step 5 (terminal eval).
/// - `exposure_delta`: Δdanger from step (worst_path_danger for Move, 0 for Cast).
/// - `geometry_gain`: 0.0 placeholder, filled in step 17 (geometry awareness).
/// - `resource_swing`: signed resource cost (negative = spent).
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    pub expected_damage: f32,
    pub p_kill_now: f32,
    pub p_kill_soon: f32,
    pub deny_value: f32,
    pub rescue_value: f32,
    pub board_pressure: f32,
    pub exposure_delta: f32,
    pub geometry_gain: f32,
    pub resource_swing: f32,
}

/// Per-plan annotation bundle. Grows as pipeline stages accrue data
/// (outcome in wave 1; critics / band / agenda in later waves).
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlanAnnotation {
    /// One ActionOutcomeEstimate per plan step, same length as TurnPlan.steps
    /// and TurnPlan.outcomes.
    #[serde(default)]
    pub outcomes: Vec<ActionOutcomeEstimate>,
}

// ---------------------------------------------------------------------------
// Extraction helpers (step 4.2)
// ---------------------------------------------------------------------------

/// Kill-promised component extracted from `factors::offensive::split_kill`.
///
/// Returns `1.0` if `def`'s direct damage won't kill `target` now but the
/// accumulated DoT (pending on target + newly applied by this ability) will.
/// Returns `0.0` otherwise.
///
/// Invariant: same formula as `split_kill`'s `kill_promised` branch — callers
/// in both `split_kill` and generator must produce bit-equal results.
pub fn estimate_kill_soon(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
) -> f32 {
    let Some(calc) = def.effect.calc(caster) else { return 0.0 };
    let armor = if calc.pierces_armor {
        0.0
    } else {
        (target.armor + target.armor_bonus) as f32
    };
    let net = calc.expected().round() - armor + target.damage_taken_bonus as f32;
    // kill_now case — no kill_soon when net already kills
    if net >= target.hp as f32 {
        return 0.0;
    }
    let pending_dot = already_pending_dot(target);
    let new_dot = dot_tick_sum_for_ability(def, target, content);
    if net + pending_dot + new_dot >= target.hp as f32 { 1.0 } else { 0.0 }
}

/// Denial value from CC statuses applied by `def` against `target`.
///
/// Extracted from `factors::offensive::status_cc_value` — formula is 1:1.
/// Includes stun denial (via `stun_denial_value`) plus vulnerability and
/// armor-shred contributions.
pub fn estimate_deny_value(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    let stun = stun_denial_value(def, target, content);
    let other: f32 = status_applications(def, content)
        .map(|(sd, d)| {
            let mut val = 0.0f32;
            if sd.damage_taken_bonus > 0 {
                val += sd.damage_taken_bonus as f32 * d;
            }
            if sd.armor_bonus > 0 {
                val += sd.armor_bonus as f32 * d;
            }
            val
        })
        .sum();
    stun + other
}

/// Heal value for a `SingleAlly` ability with urgency baked in.
///
/// Calls `scoring::score_action` (the single source of truth for heal scoring)
/// and wraps it with `crit_fail_adjusted` — exactly as
/// `factors::offensive::compute_offensive` does for the `heal` branch.
/// Returns `0.0` for non-heal or non-SingleAlly abilities.
pub fn estimate_rescue_value(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
    crit_fail_effect: &CritFailEffect,
    crit_fail_chance: f32,
) -> f32 {
    if def.target_type != TargetType::SingleAlly {
        return 0.0;
    }
    let raw = score_action(def, target, caster, content, danger_at_target);
    // Apply crit-fail adjustment (same as compute_offensive wraps score_action).
    crit_fail_adjusted_rescue(raw, def, crit_fail_effect, crit_fail_chance)
}

/// Max danger value along the path tiles of a single Move step.
/// Returns `0.0` for Cast steps.
///
/// Shared helper for `exposure_delta` in the outcome estimate. Uses only the
/// current step's path (not the whole plan) so each step's annotation is
/// independent.
pub fn step_path_danger(step: &PlanStep, maps: &InfluenceMaps) -> f32 {
    let PlanStep::Move { path } = step else { return 0.0 };
    path.iter().map(|&h| maps.danger.get(h)).fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// Private helpers (mirrors of private fns in factors::offensive)
// ---------------------------------------------------------------------------

fn already_pending_dot(target: &UnitSnapshot) -> f32 {
    target
        .statuses
        .iter()
        .map(|s| s.dot_per_tick.max(0) as f32 * s.rounds_remaining as f32)
        .sum()
}

fn dot_tick_sum_for_ability(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    status_applications(def, content)
        .map(|(sd, dur)| {
            let per_tick = sd.dot_dice.as_ref().map(|d| d.expected()).unwrap_or(0.0)
                + sd.hp_percent_dot as f32 / 100.0 * target.max_hp as f32;
            per_tick * dur
        })
        .filter(|&v| v > 0.0)
        .sum()
}

/// Crit-fail expected-value adjustment for heal (`rescue_value`).
/// Mirrors `factors::adjustments::crit_fail_adjusted` — same formula.
fn crit_fail_adjusted_rescue(
    score: f32,
    def: &AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
    use crate::core::ResourceKind;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::snapshot::ActiveStatusView;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::content_view::ContentView;
    use crate::core::{AbilityId, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn db() -> ContentView {
        ContentView::load_global_for_tests()
    }

    fn get_def<'a>(content: &'a ContentView, id: &str) -> &'a AbilityDef {
        content.abilities.get(&AbilityId::from(id)).expect("ability not found")
    }

    fn melee_caster(str_mod: i32) -> CasterContext {
        CasterContext { str_mod, ..Default::default() }
    }

    #[test]
    fn default_outcome_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.expected_damage, 0.0);
        assert_eq!(o.p_kill_now, 0.0);
        assert_eq!(o.p_kill_soon, 0.0);
        assert_eq!(o.deny_value, 0.0);
        assert_eq!(o.rescue_value, 0.0);
        assert_eq!(o.board_pressure, 0.0);
        assert_eq!(o.exposure_delta, 0.0);
        assert_eq!(o.geometry_gain, 0.0);
        assert_eq!(o.resource_swing, 0.0);
    }

    #[test]
    fn default_annotation_is_empty() {
        let a = PlanAnnotation::default();
        assert!(a.outcomes.is_empty());
    }

    // --- estimate_kill_soon ---

    /// Mirrors `kill_promised_via_pending_dot_on_target` from factors::offensive::tests.
    /// melee_attack str_mod=0 → direct=0; pending DoT 6 ≥ hp=5 → kill_soon=1.0.
    #[test]
    fn estimate_kill_soon_matches_split_kill_promised_pending_dot() {
        let content = db();
        let mut target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(5).build();
        target.statuses = vec![ActiveStatusView {
            id: StatusId::from("poisoned"),
            rounds_remaining: 2,
            dot_per_tick: 3,
        }];
        let kp = estimate_kill_soon(get_def(&content, "melee_attack"), &target, &melee_caster(0), &content);
        assert_eq!(kp, 1.0, "pending DoT 6 >= hp=5 -> kill_soon=1.0");
    }

    /// When direct damage kills (kill_now case), kill_soon must be 0.
    #[test]
    fn estimate_kill_soon_zero_when_direct_kills() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).hp(1).build();
        let kp = estimate_kill_soon(get_def(&content, "melee_attack"), &target, &melee_caster(2), &content);
        assert_eq!(kp, 0.0, "kill_now case -> kill_soon must be 0");
    }

    // --- estimate_deny_value ---

    /// stun_denial_value test: ability with skips_turn status should produce > 0 deny.
    /// Uses poison_shot as a proxy for an ability that applies statuses.
    /// For a pure CC scenario, use stun ability when available in test content.
    #[test]
    fn estimate_deny_value_zero_for_no_cc_ability() {
        let content = db();
        // melee_attack has no status effects -> deny_value = 0
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_deny_value(get_def(&content, "melee_attack"), &target, &content);
        assert_eq!(val, 0.0, "melee_attack applies no CC -> deny=0");
    }

    /// poison_shot applies poisoned status with dot — has no skips_turn or
    /// damage_taken_bonus, so deny_value = 0 (cc-denial subset only).
    #[test]
    fn estimate_deny_value_zero_for_dot_only_status() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_deny_value(get_def(&content, "poison_shot"), &target, &content);
        assert_eq!(val, 0.0, "poison_shot has DoT but no skips_turn/damage_taken_bonus -> deny=0");
    }

    // --- estimate_rescue_value ---

    /// Non-heal ability -> rescue_value = 0.
    #[test]
    fn estimate_rescue_value_zero_for_damage_ability() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_rescue_value(
            get_def(&content, "melee_attack"),
            &target,
            &CasterContext::default(),
            &content,
            0.0,
            &CritFailEffect::Miss,
            0.0,
        );
        assert_eq!(val, 0.0, "melee_attack is not a heal -> rescue=0");
    }

    /// Full-HP target -> rescue_value = 0 (no missing HP to heal).
    #[test]
    fn estimate_rescue_value_zero_for_full_hp_target() {
        let content = db();
        // full_hp means hp == max_hp, missing = 0
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(20).build();
        // Use heal ability from content if available, otherwise skip gracefully.
        if let Some(def) = content.abilities.get(&AbilityId::from("heal")) {
            let val = estimate_rescue_value(
                def, &target, &CasterContext::default(), &content, 0.0,
                &CritFailEffect::Miss, 0.0,
            );
            assert_eq!(val, 0.0, "full-HP target -> rescue=0");
        }
    }

    // --- step_path_danger ---

    fn empty_maps_local() -> crate::combat::ai::influence::InfluenceMaps {
        use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    /// Cast step -> exposure_delta = 0.
    #[test]
    fn step_path_danger_zero_for_cast() {
        use bevy::prelude::Entity;
        let maps = empty_maps_local();
        let step = PlanStep::Cast {
            ability: crate::core::AbilityId::from("melee_attack"),
            target: Entity::from_bits(1),
            target_pos: hex_from_offset(0, 0),
        };
        assert_eq!(step_path_danger(&step, &maps), 0.0);
    }

    /// Move through tiles with known danger -> max is returned.
    #[test]
    fn step_path_danger_returns_max_along_path() {
        use crate::game::hex::hex_from_offset;
        let mut maps = empty_maps_local();
        let h1 = hex_from_offset(0, 1);
        let h2 = hex_from_offset(0, 2);
        maps.danger.add(h1, 3.0);
        maps.danger.add(h2, 7.0);
        let step = PlanStep::Move { path: vec![h1, h2] };
        assert_eq!(step_path_danger(&step, &maps), 7.0);
    }
}
