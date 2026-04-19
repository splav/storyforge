//! Offensive factors: damage / heal / kill / cc for single-target and AoE.

#![allow(clippy::too_many_arguments)]

use super::adjustments::crit_fail_adjusted;
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
        let damage = compute_aoe_damage(def, &area, active, ctx, snap);
        let hit_enemies: Vec<&UnitSnapshot> = snap
            .enemies_of(active.team)
            .filter(|e| area.contains(&e.pos))
            .collect();
        let kill = if hit_enemies.iter().any(|e| single_target_kill(def, e, ctx) > 0.0) {
            1.0
        } else {
            0.0
        };
        let cc: f32 = hit_enemies
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

fn compute_aoe_damage(
    def: &AbilityDef,
    area: &HashSet<Hex>,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let mut damage = 0.0f32;
    for enemy in snap.enemies_of(active.team) {
        if area.contains(&enemy.pos) {
            damage += score_action(def, enemy, ctx.actor.caster, ctx.world.content);
        }
    }
    if def.friendly_fire {
        for ally in snap.allies_of(active.team) {
            if area.contains(&ally.pos) {
                let raw = score_action(def, ally, ctx.actor.caster, ctx.world.content).abs();
                let hp_fraction = raw / ally.max_hp.max(1) as f32;
                damage -= raw * (1.0 + hp_fraction);
            }
        }
        if area.contains(&active.pos) {
            let raw = score_action(def, active, ctx.actor.caster, ctx.world.content).abs();
            let hp_fraction = raw / active.max_hp.max(1) as f32;
            damage -= raw * (1.0 + hp_fraction);
        }
    }
    crit_fail_adjusted(damage, def, &ctx.actor.crit_fail_effect, ctx.actor.crit_fail_chance)
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
