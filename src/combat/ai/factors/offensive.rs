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

    let (damage, heal, kill, cc) = if def.aoe == AoEShape::None {
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
        let kill = match target_unit {
            Some(t) if def.target_type == TargetType::SingleEnemy => {
                single_target_kill(def, t, caster)
            }
            _ => 0.0,
        };
        let cc = target_unit.map_or(0.0, |t| status_cc_value(def, t, content));
        (damage, heal, kill, cc)
    } else {
        let area = aoe_area(def, target_pos, caster_tile);
        let hits = aoe_hits(&area, active, snap);
        let damage = compute_aoe_damage(
            def, &hits, active, caster, content, crit_fail_effect, crit_fail_chance,
        );
        let kill = if hits
            .enemies
            .iter()
            .any(|e| single_target_kill(def, e, caster) > 0.0)
        {
            1.0
        } else {
            0.0
        };
        let cc: f32 = hits
            .enemies
            .iter()
            .map(|e| status_cc_value(def, e, content))
            .sum();
        (damage, 0.0, kill, cc)
    };

    OffensiveFactors { damage, heal, kill, cc }
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

/// Does `def`'s expected damage overkill `target`? Returns 1.0 or 0.0.
fn single_target_kill(def: &AbilityDef, target: &UnitSnapshot, caster: &CasterContext) -> f32 {
    let Some(calc) = def.effect.calc(caster) else { return 0.0 };
    let armor = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
    let net = calc.expected() - armor + target.damage_taken_bonus as f32;
    if net >= target.hp as f32 { 1.0 } else { 0.0 }
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
