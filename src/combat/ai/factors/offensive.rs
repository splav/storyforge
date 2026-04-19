//! Offensive factors: damage / heal / kill / cc for single-target and AoE.

#![allow(clippy::too_many_arguments)]

use super::adjustments::crit_fail_adjusted;
use super::aoe_hits::{aoe_hits, AoeHits};
use super::OffensiveFactors;
use crate::combat::ai::scoring::score_action;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::UtilityContext;
use crate::combat::effects_math::aoe_cells;
use crate::content::abilities::{AbilityDef, AoEShape, EffectDef, TargetType};
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::HashSet;

pub(super) fn compute_offensive(
    ability: &AbilityId,
    target_pos: Hex,
    target: Entity,
    caster_tile: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> OffensiveFactors {
    let Some(def) = ctx.world.content.abilities.get(ability) else {
        return OffensiveFactors::default();
    };

    if matches!(def.effect, EffectDef::Summon { .. }) {
        return OffensiveFactors::default();
    }

    let (damage, heal, kill, cc) = if def.aoe == AoEShape::None {
        let mut damage = 0.0f32;
        let mut heal = 0.0f32;
        let target_unit = snap.unit(target);
        if let Some(target_unit) = target_unit {
            let raw = score_action(def, target_unit, ctx.actor.caster, ctx.world.content);
            let adjusted = crit_fail_adjusted(raw, def, &ctx.actor.crit_fail_effect, ctx.actor.crit_fail_chance);
            if def.target_type == TargetType::SingleAlly {
                heal = adjusted;
            } else if def.target_type == TargetType::SingleEnemy {
                damage = adjusted;
            }
        }
        let kill = match target_unit {
            Some(t) if def.target_type == TargetType::SingleEnemy => single_target_kill(def, t, ctx),
            _ => 0.0,
        };
        let cc = target_unit.map_or(0.0, |t| status_cc_value(def, t.threat, ctx));
        (damage, heal, kill, cc)
    } else {
        let area = aoe_area(def, target_pos, caster_tile);
        let hits = aoe_hits(&area, active, snap);
        let damage = compute_aoe_damage(def, &hits, active, ctx);
        let kill = if hits.enemies.iter().any(|e| single_target_kill(def, e, ctx) > 0.0) {
            1.0
        } else {
            0.0
        };
        let cc: f32 = hits
            .enemies
            .iter()
            .map(|e| status_cc_value(def, e.threat, ctx))
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
fn friendly_fire_penalty(def: &AbilityDef, u: &UnitSnapshot, ctx: &UtilityContext) -> f32 {
    let raw = score_action(def, u, ctx.actor.caster, ctx.world.content).abs();
    raw * (1.0 + raw / u.max_hp.max(1) as f32)
}

/// Net AoE damage = enemies hit minus friendly-fire splash, crit-fail-adjusted.
///
/// `hits.allies` excludes the actor — `self_hit` carries it separately — so
/// chaining the two iterators penalises the caster at most once even when
/// they stand in their own blast. Before this consolidation, iterating
/// `allies_of(team)` (which includes self) plus an explicit self-branch
/// subtracted self-damage twice.
fn compute_aoe_damage(
    def: &AbilityDef,
    hits: &AoeHits,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
) -> f32 {
    let enemy_damage: f32 = hits
        .enemies
        .iter()
        .map(|e| score_action(def, e, ctx.actor.caster, ctx.world.content))
        .sum();
    let splash: f32 = if def.friendly_fire {
        hits.allies
            .iter()
            .copied()
            .chain(hits.self_hit.then_some(active))
            .map(|u| friendly_fire_penalty(def, u, ctx))
            .sum()
    } else {
        0.0
    };
    crit_fail_adjusted(
        enemy_damage - splash,
        def,
        &ctx.actor.crit_fail_effect,
        ctx.actor.crit_fail_chance,
    )
}

/// Does `def`'s expected damage overkill `target`? Returns 1.0 or 0.0.
fn single_target_kill(def: &AbilityDef, target: &UnitSnapshot, ctx: &UtilityContext) -> f32 {
    let Some(calc) = def.effect.calc(ctx.actor.caster) else { return 0.0 };
    let armor = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
    let net = calc.expected() - armor + target.damage_taken_bonus as f32;
    if net >= target.hp as f32 { 1.0 } else { 0.0 }
}

/// Sum the CC value contribution of `def`'s statuses against a unit with the
/// given `threat`. Used per-target for single-target and per-enemy for AoE.
fn status_cc_value(def: &AbilityDef, threat: f32, ctx: &UtilityContext) -> f32 {
    def.statuses
        .iter()
        .map(|sa| {
            let Some(sd) = ctx.world.content.statuses.get(&sa.status) else { return 0.0 };
            let d = sa.duration_rounds as f32;
            let mut val = 0.0f32;
            if sd.skips_turn {
                val += threat * d;
            }
            if sd.damage_taken_bonus > 0 { val += sd.damage_taken_bonus as f32 * d; }
            if sd.armor_bonus > 0 { val += sd.armor_bonus as f32 * d; }
            val
        })
        .sum()
}
