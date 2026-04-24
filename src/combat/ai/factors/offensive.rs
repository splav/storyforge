//! Offensive factors: damage / heal / kill / cc for single-target and AoE.
//!
//! Step 4.3: `compute_offensive` reads pre-computed values from
//! `ActionOutcomeEstimate` (annotated by the generator) instead of re-deriving
//! them. AoE friendly-fire penalty still uses `score_action` directly because it
//! operates on ally units that are not captured in the outcome vector.

use super::aoe_hits::{aoe_hits, AoeHits};
use super::{crit_fail_adjusted, OffensiveFactors};
use crate::combat::ai::outcome::ActionOutcomeEstimate;
#[allow(deprecated)]
use crate::combat::ai::scoring::score_action;

use crate::combat::ai::snapshot::UnitSnapshot;
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::effects_math::aoe_cells;
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectDef};
use crate::content::content_view::ContentView;
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::HashSet;

/// Compute offensive factors for a single Cast step.
///
/// Reads pre-annotated values from `outcome` (filled by the generator's
/// `build_step_outcome_estimate`) rather than re-deriving them from scratch.
/// This makes the scorer a pure reader of the annotation vector.
///
/// The only live computation remaining here is the **AoE friendly-fire** penalty
/// via `compute_aoe_damage` — it operates on ally units which are not captured
/// in `ActionOutcomeEstimate`, so it cannot be read from the outcome directly.
pub(super) fn compute_offensive(
    ability: &AbilityId,
    target_pos: Hex,
    _target: Entity,
    caster_tile: Hex,
    ctx: &ScoringCtx,
    outcome: &ActionOutcomeEstimate,
) -> OffensiveFactors {
    let content = ctx.world.content;
    let Some(def) = content.abilities.get(ability) else {
        return OffensiveFactors::default();
    };

    if matches!(def.effect, EffectDef::Summon { .. }) {
        return OffensiveFactors::default();
    }

    let snap = ctx.snap;
    let active = ctx.active;
    let caster = &active.caster_ctx;
    let crit_fail_effect = &active.crit_fail_effect;
    let crit_fail_chance = ctx.world.crit_fail_chance;

    // Read kill signals and CC directly from the pre-annotated outcome.
    let kill_now = outcome.p_kill_now;
    let kill_promised = outcome.p_kill_soon;
    let cc = outcome.deny_value;
    let heal = outcome.rescue_value;

    // For damage: outcome.expected_damage holds total damage (enemy hits minus
    // friendly-fire, crit-fail-adjusted) for single-target casts. For AoE,
    // outcome.expected_damage is sim-derived total damage, but we need to
    // re-apply the friendly-fire penalty which is not captured in the outcome.
    let damage = if def.aoe == AoEShape::None {
        // Single-target: read directly from outcome.
        outcome.expected_damage
    } else {
        // AoE: the sim's outcome.damage already includes friendly-fire netting
        // (sim applies splash to allies too). Use expected_damage as-is — it
        // matches what compute_aoe_damage would produce via the old path.
        // The AoE friendly-fire branch below re-checks the ratio for the
        // `adjustments` pass (reservation nerf), but damage itself reads outcome.
        let area = aoe_area(def, target_pos, caster_tile);
        let hits = aoe_hits(&area, active, snap);
        // Re-derive AoE damage using the original helper, which accounts for
        // friendly-fire splash correctly (ally hits are not in the outcome).
        compute_aoe_damage(def, &hits, active, caster, content, crit_fail_effect, crit_fail_chance)
    };

    OffensiveFactors { damage, heal, kill_now, kill_promised, cc }
}

/// Expand an AoE def into the set of affected tiles. Thin wrapper over
/// `effects_math::aoe_cells` that materialises the result as a `HashSet` for
/// fast `contains` checks in the planner.
pub fn aoe_area(def: &AbilityDef, target_pos: Hex, caster_tile: Hex) -> HashSet<Hex> {
    aoe_cells(def.aoe, caster_tile, target_pos).into_iter().collect()
}

/// `raw × (1 + raw/max_hp)` — punishes plans that chunk a non-enemy's HP%
/// harder, so a fireball on a full-HP ally is worse than on a nicked one.
#[allow(deprecated)] // score_action: legacy path, removed in step 4.5
fn friendly_fire_penalty(
    def: &AbilityDef,
    u: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
) -> f32 {
    // Friendly-fire splash is a damage estimate; heal-urgency is irrelevant.
    let raw = score_action(def, u, caster, content, 0.0).abs();
    raw * (1.0 + raw / u.max_hp.max(1) as f32)
}

/// Net AoE damage = enemies hit minus friendly-fire splash, crit-fail-adjusted.
///
/// `hits.allies` excludes the actor — `self_hit` carries it separately — so
/// chaining the two iterators penalises the caster at most once even when
/// they stand in their own blast. Before this consolidation, iterating
/// `allies_of(team)` (which includes self) plus an explicit self-branch
/// subtracted self-damage twice.
#[allow(clippy::too_many_arguments)]
#[allow(deprecated)] // score_action: legacy path, removed in step 4.5
fn compute_aoe_damage(
    def: &AbilityDef,
    hits: &AoeHits,
    active: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
    crit_fail_effect: &crate::content::races::CritFailEffect,
    crit_fail_chance: f32,
) -> f32 {
    // AoE damage path never triggers heal urgency (not SingleAlly).
    let enemy_damage: f32 = hits
        .enemies
        .iter()
        .map(|e| score_action(def, e, caster, content, 0.0))
        .sum();
    let splash: f32 = if def.friendly_fire {
        hits.allies
            .iter()
            .copied()
            .chain(hits.self_hit.then_some(active))
            .map(|u| friendly_fire_penalty(def, u, caster, content))
            .sum()
    } else {
        0.0
    };
    crit_fail_adjusted(enemy_damage - splash, def, crit_fail_effect, crit_fail_chance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::{estimate_deny_value, estimate_kill_soon};
    use crate::combat::ai::snapshot::ActiveStatusView;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::abilities::CasterContext;
    use crate::content::content_view::ContentView;
    use crate::core::{AbilityId, DiceExpr, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Returns `(kill_now, kill_promised)`:
    /// - `kill_now = 1.0` if direct expected damage kills the target this cast.
    /// - `kill_promised = 1.0` if direct damage won't kill alone but accumulated
    ///   DoT (pending on target + newly applied by this ability) will finish it.
    ///
    /// Invariant: at most one is 1.0; `kill_now = 1` implies `kill_promised = 0`.
    ///
    /// `kill_promised` delegates to `outcome::estimate_kill_soon` so both callers
    /// share exactly one formula. Kept in test scope only — step 4.3 moved live
    /// scoring to read from `ActionOutcomeEstimate` directly.
    fn split_kill(
        def: &AbilityDef,
        target: &UnitSnapshot,
        caster: &CasterContext,
        content: &ContentView,
    ) -> (f32, f32) {
        let Some(calc) = def.effect.calc(caster) else { return (0.0, 0.0) };
        let armor = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
        let net = calc.expected().round() - armor + target.damage_taken_bonus as f32;
        if net >= target.hp as f32 {
            return (1.0, 0.0);
        }
        (0.0, estimate_kill_soon(def, target, caster, content))
    }

    /// Thin wrapper used in tests only. Delegates to `estimate_deny_value` —
    /// kept for test clarity, removed from prod in step 4.3.
    #[allow(dead_code)]
    fn status_cc_value(def: &AbilityDef, target: &UnitSnapshot, content: &ContentView) -> f32 {
        estimate_deny_value(def, target, content)
    }

    fn db() -> ContentView {
        ContentView::load_global_for_tests()
    }

    fn get_def<'a>(content: &'a ContentView, id: &str) -> &'a AbilityDef {
        content.abilities.get(&AbilityId::from(id)).expect("ability not found in test content")
    }

    fn melee_caster(str_mod: i32) -> CasterContext {
        CasterContext { str_mod, ..Default::default() }
    }

    /// melee_attack (WeaponAttack, bonus=str_mod, no dice): str_mod=2 → direct=2 ≥ hp=1
    #[test]
    fn kill_now_when_direct_damage_kills() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).hp(1).build();
        let (kn, kp) = split_kill(get_def(&content, "melee_attack"), &target, &melee_caster(2), &content);
        assert_eq!(kn, 1.0, "kill_now should fire");
        assert_eq!(kp, 0.0, "kill_promised must be 0 when kill_now=1");
    }

    /// melee_attack with str_mod=0 → direct=0; pending DoT (3/tick × 2 rounds = 6) ≥ hp=5
    #[test]
    fn kill_promised_via_pending_dot_on_target() {
        let content = db();
        let mut target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(5).build();
        target.statuses = vec![ActiveStatusView {
            id: StatusId::from("poisoned"),
            rounds_remaining: 2,
            dot_per_tick: 3,
        }];
        let (kn, kp) = split_kill(get_def(&content, "melee_attack"), &target, &melee_caster(0), &content);
        assert_eq!(kn, 0.0, "direct=0, no kill_now");
        assert_eq!(kp, 1.0, "pending DoT 6 ≥ hp=5 → kill_promised");
    }

    /// poison_shot: direct 1d4 (expected 2.5) + poisoned×3 (2.5/tick × 3 = 7.5) = 10 ≥ hp=5
    #[test]
    fn kill_promised_via_new_dot_from_ability() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(5).build();
        let c = CasterContext::default();
        let (kn, kp) = split_kill(get_def(&content, "poison_shot"), &target, &c, &content);
        assert_eq!(kn, 0.0, "direct=2.5 does not kill hp=5");
        assert_eq!(kp, 1.0, "direct+DoT=10 kills hp=5");
    }

    /// melee_attack with str_mod=0, no pending DoT: direct=0, combined=0 < hp=100
    #[test]
    fn no_kill_when_combined_insufficient() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(100).build();
        let (kn, kp) = split_kill(get_def(&content, "melee_attack"), &target, &melee_caster(0), &content);
        assert_eq!(kn, 0.0);
        assert_eq!(kp, 0.0);
    }

    /// Boundary case: WeaponAttack 1d6 + str_mod=2 → expected=5.5, sim rounds to 6.
    /// Target hp=6, armor=0. Scorer must match sim: kill_now=1, not 0.
    #[test]
    fn split_kill_rounds_expected_to_match_sim() {
        let content = db();
        let caster = CasterContext {
            str_mod: 2,
            weapon_dice: Some(DiceExpr::new(1, 6, 0)),
            ..Default::default()
        };
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).hp(6).build();
        let (kn, kp) = split_kill(get_def(&content, "melee_attack"), &target, &caster, &content);
        assert_eq!(kn, 1.0, "kill_now must be 1: expected()=5.5 rounds to 6 >= hp=6");
        assert_eq!(kp, 0.0, "kill_promised must be 0 when kill_now=1");
    }

    /// Step 4.3 contract: `compute_offensive` must read `outcome.expected_damage`,
    /// `outcome.p_kill_soon`, and `outcome.deny_value` directly, not re-derive them.
    ///
    /// We inject a synthetic outcome with known values and assert the returned
    /// `OffensiveFactors` mirrors them exactly. If any field were re-derived from
    /// snapshot/score_action the values would differ.
    #[test]
    fn compute_offensive_reads_outcome_not_score_action() {
        use crate::combat::ai::difficulty::DifficultyProfile;
        use crate::combat::ai::reservations::Reservations;
        use crate::combat::ai::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};

        let content = db();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);
        // High-HP target so no kill_now from snapshot.
        let actor = UnitBuilder::new(1, Team::Enemy, caster_pos).full_hp(100).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).full_hp(100).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // Inject a synthetic outcome with known sentinel values.
        let outcome = ActionOutcomeEstimate {
            expected_damage: 100.0,
            p_kill_now: 0.0,
            p_kill_soon: 1.0,
            deny_value: 42.0,
            rescue_value: 0.0,
            ..Default::default()
        };

        let ability = AbilityId::from("melee_attack");
        let off = compute_offensive(&ability, target_pos, target.entity, caster_pos, &ctx, &outcome);

        assert_eq!(off.damage, 100.0, "damage must come from outcome.expected_damage");
        assert_eq!(off.kill_promised, 1.0, "kill_promised must come from outcome.p_kill_soon");
        assert_eq!(off.cc, 42.0, "cc must come from outcome.deny_value");
        assert_eq!(off.kill_now, 0.0, "kill_now must come from outcome.p_kill_now");
    }
}
