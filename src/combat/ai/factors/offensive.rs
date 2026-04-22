//! Offensive factors: damage / heal / kill / cc for single-target and AoE.

use super::adjustments::crit_fail_adjusted;
use super::aoe_hits::{aoe_hits, AoeHits};
use super::OffensiveFactors;
use crate::combat::ai::scoring::{
    score_action, status_applications, stun_denial_value,
};
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::effects_math::aoe_cells;
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectDef, TargetType};
use crate::content::content_view::ContentView;
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::HashSet;

pub(super) fn compute_offensive(
    ability: &AbilityId,
    target_pos: Hex,
    target: Entity,
    caster_tile: Hex,
    ctx: &ScoringCtx,
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
    // Per-actor casting facts now live on the snapshot row; read them once
    // so downstream helpers stay ignorant of `ScoringCtx`.
    let caster = &active.caster_ctx;
    let crit_fail_effect = &active.crit_fail_effect;
    let crit_fail_chance = ctx.world.crit_fail_chance;
    // Danger at the target tile — feeds heal-urgency weighting inside
    // `score_action`. Damage paths ignore it.
    let danger_at_target = ctx.maps.danger.get(target_pos);

    let (damage, heal, kill_now, kill_promised, cc) = if def.aoe == AoEShape::None {
        let mut damage = 0.0f32;
        let mut heal = 0.0f32;
        let target_unit = snap.unit(target);
        if let Some(target_unit) = target_unit {
            let raw = score_action(def, target_unit, caster, content, danger_at_target);
            let adjusted = crit_fail_adjusted(raw, def, crit_fail_effect, crit_fail_chance);
            if def.target_type == TargetType::SingleAlly {
                heal = adjusted;
            } else if def.target_type == TargetType::SingleEnemy {
                damage = adjusted;
            }
        }
        let (kill_now, kill_promised) = match target_unit {
            Some(t) if def.target_type == TargetType::SingleEnemy => {
                split_kill(def, t, caster, content)
            }
            _ => (0.0, 0.0),
        };
        let cc = target_unit.map_or(0.0, |t| status_cc_value(def, t, content));
        (damage, heal, kill_now, kill_promised, cc)
    } else {
        let area = aoe_area(def, target_pos, caster_tile);
        let hits = aoe_hits(&area, active, snap);
        let damage = compute_aoe_damage(
            def, &hits, active, caster, content, crit_fail_effect, crit_fail_chance,
        );
        let any_kill_now = hits.enemies.iter().any(|e| {
            let (kn, _) = split_kill(def, e, caster, content);
            kn > 0.0
        });
        let (kill_now, kill_promised) = if any_kill_now {
            (1.0, 0.0)
        } else {
            let any_promised = hits.enemies.iter().any(|e| {
                let (_, kp) = split_kill(def, e, caster, content);
                kp > 0.0
            });
            (0.0, if any_promised { 1.0 } else { 0.0 })
        };
        let cc: f32 = hits
            .enemies
            .iter()
            .map(|e| status_cc_value(def, e, content))
            .sum();
        (damage, 0.0, kill_now, kill_promised, cc)
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

/// Returns `(kill_now, kill_promised)`:
/// - `kill_now = 1.0` if direct expected damage kills the target this cast.
/// - `kill_promised = 1.0` if direct damage won't kill alone but accumulated
///   DoT (pending on target + newly applied by this ability) will finish it.
/// Invariant: at most one is 1.0; `kill_now = 1` implies `kill_promised = 0`.
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
    let pending_dot = already_pending_dot(target);
    let new_dot = dot_tick_sum_for_ability(def, target, content);
    if net + pending_dot + new_dot >= target.hp as f32 { (0.0, 1.0) } else { (0.0, 0.0) }
}

/// Sum of expected DoT damage from `def`'s status applications over their
/// full durations. Positive-clamped per application so heal-over-time statuses
/// don't reduce the total.
fn dot_tick_sum_for_ability(def: &AbilityDef, target: &UnitSnapshot, content: &ContentView) -> f32 {
    status_applications(def, content)
        .map(|(sd, dur)| {
            let per_tick = sd.dot_dice.as_ref().map(|d| d.expected()).unwrap_or(0.0)
                + sd.hp_percent_dot as f32 / 100.0 * target.max_hp as f32;
            per_tick * dur
        })
        .filter(|&v| v > 0.0)
        .sum()
}

/// Expected total DoT damage already pending on `target` from existing statuses.
fn already_pending_dot(target: &UnitSnapshot) -> f32 {
    target
        .statuses
        .iter()
        .map(|s| s.dot_per_tick.max(0) as f32 * s.rounds_remaining as f32)
        .sum()
}

/// Sum the CC-denial value of `def`'s statuses against `target`. Used
/// per-target for single-target casts and per-enemy summed for AoE.
///
/// Differs from `scoring::status_score` deliberately: this is the CC-factor
/// denial subset — counts only `skips_turn`, positive `damage_taken_bonus`,
/// positive `armor_bonus` (the "bad-for-target" direction, because the
/// factor is meaningful only on enemies). `status_score` uses `.abs()` so
/// it can price ally buffs too.
///
/// Stun denial (skips_turn) goes through the shared `stun_denial_value`
/// helper — same formula the `scarcity` swing branch reads, so the two
/// stay in lockstep. Vulnerability / armor-shred contributions fold in
/// separately here because the scarcity branch doesn't consider them.
fn status_cc_value(def: &AbilityDef, target: &UnitSnapshot, content: &ContentView) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::snapshot::ActiveStatusView;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::abilities::CasterContext;
    use crate::content::content_view::ContentView;
    use crate::core::{AbilityId, DiceExpr, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

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
}
