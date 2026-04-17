//! 9-factor scoring pipeline.
//!
//! Takes a candidate pool and produces one composite score per candidate by
//! combining `[damage, kill, cc, heal, position, risk, focus, intent, scarcity]`
//! with role-axis weights, per-factor difficulty multipliers, and noise.
//!
//! `score_candidates` is the public entry. `compute_factors` is exposed so the
//! debug printer can display the raw per-factor breakdown for top-K candidates.

#![allow(clippy::too_many_arguments)]

use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{intent_score, TacticalIntent};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::{applies_cc, score_action};
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{AoEShape, EffectDef, TargetType};
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng, ResourceKind};
use crate::game::hex::{hex_circle, hex_line, Hex};
use bevy::prelude::*;
use std::collections::HashSet;

// ── Factor layout ───────────────────────────────────────────────────────────

/// 9 utility factors: damage, kill, cc, heal, position, risk, focus, intent, scarcity.
pub const NUM_FACTORS: usize = 9;

/// Factors that can be negative (position, intent, scarcity).
/// These use symmetric normalization: divide by max(|min|, |max|) → [-1, 1].
/// Non-negative factors use standard max normalization → [0, 1].
const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, true, false, false, true, true,
];

/// Per-candidate offensive factors (populated only for Cast).
#[derive(Default)]
struct OffensiveFactors {
    damage: f32,
    heal: f32,
    kill: f32,
    cc: f32,
}

// ── Top-level scoring ───────────────────────────────────────────────────────

pub fn score_candidates(
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> Vec<f32> {
    if candidates.is_empty() {
        return vec![];
    }

    // Compute raw factors for each candidate.
    let raw: Vec<[f32; NUM_FACTORS]> = candidates
        .iter()
        .map(|c| compute_factors(c, active, intent, ctx, snap, maps, reservations))
        .collect();

    // Find per-factor extremes for normalization.
    let mut maxes = [0.0f32; NUM_FACTORS];
    let mut mins = [0.0f32; NUM_FACTORS];
    for factors in &raw {
        for (i, &v) in factors.iter().enumerate() {
            if v > maxes[i] { maxes[i] = v; }
            if v < mins[i] { mins[i] = v; }
        }
    }

    // Compute normalization denominator per factor.
    let mut denom = [0.0f32; NUM_FACTORS];
    for i in 0..NUM_FACTORS {
        denom[i] = if SIGNED_FACTOR[i] {
            // Symmetric: divide by max absolute value → [-1, 1]
            mins[i].abs().max(maxes[i].abs())
        } else {
            // Non-negative: divide by max → [0, 1]
            maxes[i]
        };
    }

    // Normalize and apply composed axis weights, with per-factor difficulty multipliers.
    let mut weights = active.role.factor_weights();
    // intent factor (idx 7): scaled by intent_commitment.
    weights[7] *= ctx.difficulty.intent_commitment;
    // scarcity factor (idx 8): scaled by resource_discipline.
    weights[8] *= ctx.difficulty.resource_discipline;

    let noise_amp = ctx.difficulty.score_noise();

    raw.iter()
        .map(|factors| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    factors[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }

            // Add noise.
            if noise_amp > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
                score += noise;
            }

            score
        })
        .collect()
}

/// Compute the 9 raw utility factors for a single candidate.
/// Axes: [damage, kill, cc, heal, position, risk, focus, intent, scarcity].
pub fn compute_factors(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> [f32; NUM_FACTORS] {
    let mut off = match &candidate.kind {
        CandidateKind::Cast { ability, target_pos, target } => {
            compute_offensive(ability, *target_pos, *target, candidate.tile, active, ctx, snap)
        }
        CandidateKind::MoveOnly => OffensiveFactors::default(),
    };

    let mut position = evaluate_position(candidate.tile, &active.role, maps);
    let risk = 1.0 - maps.danger.get(candidate.tile);
    let mut focus = candidate
        .target()
        .and_then(|t| snap.unit(t))
        .map(|t| target_priority(active, t, snap))
        .unwrap_or(0.0);
    let intent_val = intent_score(intent, candidate, active, snap, maps, ctx.content, ctx.difficulty);

    apply_reservation_adjustments(candidate, &mut off, &mut focus, &mut position, snap, ctx, reservations);

    let scarcity = match &candidate.kind {
        CandidateKind::Cast { .. } => compute_scarcity(candidate, active, off.kill, ctx, snap),
        CandidateKind::MoveOnly => 0.0,
    };

    [off.damage, off.kill, off.cc, off.heal, position, risk, focus, intent_val, scarcity]
}

// ── Offensive factors: damage / heal / kill / cc ────────────────────────────

fn compute_offensive(
    ability: &AbilityId,
    target_pos: Hex,
    target: Option<Entity>,
    caster_tile: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> OffensiveFactors {
    let Some(def) = ctx.content.abilities.get(ability) else {
        return OffensiveFactors::default();
    };

    if let EffectDef::Summon { max_active, .. } = &def.effect {
        return summon_value(active, *max_active, snap);
    }

    let (damage, heal, kill, cc) = if def.aoe == AoEShape::None {
        let mut damage = 0.0f32;
        let mut heal = 0.0f32;
        let target_unit = target.and_then(|t| snap.unit(t));
        if let Some(target_unit) = target_unit {
            let raw = score_action(def, target_unit, ctx.caster, ctx.content);
            let adjusted = crit_fail_adjusted(raw, def, &ctx.crit_fail_effect, ctx.crit_fail_chance);
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

/// Score a Summon ability: baseline value decayed by how many summons this
/// caster already has on the field. Routed through the `damage` factor so it
/// competes with regular offensive options regardless of role weights.
fn summon_value(
    active: &UnitSnapshot,
    max_active: Option<u32>,
    snap: &BattleSnapshot,
) -> OffensiveFactors {
    const BASE: f32 = 14.0;
    let cap = max_active.unwrap_or(3).max(1) as f32;
    let count = snap
        .units
        .iter()
        .filter(|u| u.summoner == Some(active.entity))
        .count() as f32;
    let decay = (1.0 - (count / cap)).max(0.0);
    OffensiveFactors { damage: BASE * decay, heal: 0.0, kill: 0.0, cc: 0.0 }
}

/// Expand an AoE def into the set of affected tiles.
pub fn aoe_area(
    def: &crate::content::abilities::AbilityDef,
    target_pos: Hex,
    caster_tile: Hex,
) -> HashSet<Hex> {
    match def.aoe {
        AoEShape::Circle { radius } => hex_circle(target_pos, radius).into_iter().collect(),
        AoEShape::Line { length } => hex_line(caster_tile, target_pos, length).into_iter().collect(),
        AoEShape::None => HashSet::new(),
    }
}

fn compute_aoe_damage(
    def: &crate::content::abilities::AbilityDef,
    area: &HashSet<Hex>,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let mut damage = 0.0f32;
    for enemy in snap.enemies_of(active.team) {
        if area.contains(&enemy.pos) {
            damage += score_action(def, enemy, ctx.caster, ctx.content);
        }
    }
    if def.friendly_fire {
        for ally in snap.allies_of(active.team) {
            if area.contains(&ally.pos) {
                let raw = score_action(def, ally, ctx.caster, ctx.content).abs();
                let hp_fraction = raw / ally.max_hp.max(1) as f32;
                damage -= raw * (1.0 + hp_fraction);
            }
        }
        if area.contains(&active.pos) {
            let raw = score_action(def, active, ctx.caster, ctx.content).abs();
            let hp_fraction = raw / active.max_hp.max(1) as f32;
            damage -= raw * (1.0 + hp_fraction);
        }
    }
    crit_fail_adjusted(damage, def, &ctx.crit_fail_effect, ctx.crit_fail_chance)
}

/// Does `def`'s expected damage overkill `target`? Returns 1.0 or 0.0.
fn single_target_kill(
    def: &crate::content::abilities::AbilityDef,
    target: &UnitSnapshot,
    ctx: &UtilityContext,
) -> f32 {
    let Some(calc) = def.effect.calc(ctx.caster) else { return 0.0 };
    let armor = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
    let net = calc.expected() - armor + target.damage_taken_bonus as f32;
    if net >= target.hp as f32 { 1.0 } else { 0.0 }
}

/// Sum the CC value contribution of `def`'s statuses against a unit with the
/// given `threat`. Used per-target for single-target and per-enemy for AoE.
fn status_cc_value(
    def: &crate::content::abilities::AbilityDef,
    threat: f32,
    ctx: &UtilityContext,
) -> f32 {
    def.statuses
        .iter()
        .map(|sa| {
            let Some(sd) = ctx.content.statuses.get(&sa.status) else { return 0.0 };
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

// ── Reservation-based adjustments ───────────────────────────────────────────

/// Coordination knob: overkill penalty + focus-fire bonus + duplicate-CC + tile collision.
fn apply_reservation_adjustments(
    candidate: &ActionCandidate,
    off: &mut OffensiveFactors,
    focus: &mut f32,
    position: &mut f32,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
    reservations: &Reservations,
) {
    if let Some(target_ent) = candidate.target() {
        let reserved_dmg = reservations.reserved_damage(target_ent);
        if reserved_dmg > 0.0 {
            if let Some(target_unit) = snap.unit(target_ent) {
                let hp_left = target_unit.hp as f32 - reserved_dmg;
                if hp_left <= 0.0 {
                    off.damage *= ctx.difficulty.overkill_damage_multiplier();
                    off.kill = 0.0;
                } else {
                    *focus *= 1.0 + ctx.difficulty.focus_fire_bonus();
                }
            }
        }
        if reservations.has_reserved_cc(target_ent) {
            off.cc *= 0.15;
        }
    }
    if reservations.is_tile_reserved(candidate.tile) {
        *position *= 0.5;
    }
}

// ── Scarcity ────────────────────────────────────────────────────────────────

/// Compute resource-scarcity factor: `swing_value - resource_ratio`.
/// Free abilities return 0.0 (neutral). Expensive abilities on low-value
/// situations get negative scores; expensive abilities in high-swing moments
/// get positive scores.
fn compute_scarcity(
    candidate: &ActionCandidate,
    active: &UnitSnapshot,
    kill: f32,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let CandidateKind::Cast { ability, target_pos, target } = &candidate.kind else {
        return 0.0;
    };
    let Some(def) = ctx.content.abilities.get(ability) else {
        return 0.0;
    };

    // Free abilities are always neutral.
    if def.costs.is_empty() {
        return 0.0;
    }

    // resource_ratio: max(cost / current_pool) across all resource costs.
    let resource_ratio = def
        .costs
        .iter()
        .map(|c| {
            let pool = match c.resource {
                ResourceKind::Hp => active.hp,
                ResourceKind::Mana => active.mana.map(|(cur, _)| cur).unwrap_or(0),
                ResourceKind::Rage => active.rage.map(|(cur, _)| cur).unwrap_or(0),
                ResourceKind::Energy => active.energy.map(|(cur, _)| cur).unwrap_or(0),
            };
            if pool <= 0 {
                return 1.0;
            }
            (c.amount as f32 / pool as f32).min(1.0)
        })
        .fold(0.0f32, f32::max);

    // swing_value: situational justification for spending.
    let mut swing = 0.0f32;

    let target_unit = target.and_then(|t| snap.unit(t));

    // Kill bonus.
    if kill > 0.0 {
        swing += 0.8;
        // Extra value for killing high-value targets. For AoE (no single target),
        // credit the highest-value enemy hit — that's the kill the factor captures.
        let victim = target_unit.or_else(|| {
            if def.aoe == AoEShape::None { return None; }
            let area = aoe_area(def, *target_pos, candidate.tile);
            snap.enemies_of(active.team)
                .filter(|e| area.contains(&e.pos))
                .max_by(|a, b| {
                    a.role.role_value()
                        .partial_cmp(&b.role.role_value())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        if let Some(t) = victim {
            // Role-based kill bonus scales with target's priority value
            // (Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3).
            swing += 0.35 * t.role.role_value();
        }
    }

    // AoE multi-hit bonus.
    if def.aoe != AoEShape::None {
        let area = aoe_area(def, *target_pos, candidate.tile);
        let hits = snap
            .enemies_of(active.team)
            .filter(|e| area.contains(&e.pos))
            .count();
        if hits > 1 {
            swing += 0.2 * (hits - 1) as f32;
        }
    }

    // CC on high-threat unstunned target. Non-AoE only — AoE CC is already
    // folded into the cc factor per-enemy.
    if applies_cc(def, ctx.content) {
        if let Some(t) = target_unit {
            if !t.tags.contains(AiTags::IS_STUNNED) {
                swing += 0.5 * (t.threat / 10.0).min(1.0);
            }
        }
    }

    // Overkill penalty: target nearly dead and caster has free attacks.
    if let Some(t) = target_unit {
        if t.hp_pct() < 0.25 && has_free_attack(ctx) {
            swing -= 0.3;
        }
    }

    // Early round penalty: conserve resources at fight start.
    if snap.round <= 1 {
        swing -= 0.15;
    }

    (swing - resource_ratio).clamp(-1.0, 1.0)
}

/// Returns true if the caster has at least one ability with no resource cost.
fn has_free_attack(ctx: &UtilityContext) -> bool {
    ctx.abilities.0.iter().any(|id| {
        ctx.content
            .abilities
            .get(id)
            .is_some_and(|d| d.costs.is_empty() && d.target_type == TargetType::SingleEnemy)
    })
}

// ── Crit-fail expected-value adjustment ─────────────────────────────────────

pub fn crit_fail_adjusted(
    score: f32,
    def: &crate::content::abilities::AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
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
    use crate::content::content_view::ContentView;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::content::abilities::CasterContext;
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::hex_from_offset;
    

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(id).expect("valid entity id"),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action: true,
            movement: true,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec!["melee_attack".into()],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
            summoner: None,
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        let active = units[0].entity;
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    fn cast(tile: Hex, ability: &str, target_pos: Hex, target: Entity) -> ActionCandidate {
        ActionCandidate {
            tile,
            path: vec![],
            kind: CandidateKind::Cast {
                ability: ability.into(),
                target_pos,
                target: Some(target),
            },
        }
    }

    fn candidate(tile: Hex, target: Entity) -> ActionCandidate {
        cast(tile, "melee_attack", tile, target)
    }

    fn scarcity_ctx<'a>(
        content: &'a ContentView,
        difficulty: &'a DifficultyProfile,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        UtilityContext {
            content,
            difficulty,
            caster: &CasterContext { str_mod: 0, int_mod: 3, spell_power: 0, weapon_dice: None },
            abilities,
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        }
    }

    #[test]
    fn scarcity_neutral_for_free_abilities() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        let s = snap(vec![active.clone(), enemy.clone()]);
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = scarcity_ctx(&content, &diff, &abilities);

        let c = candidate(tile, enemy.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert_eq!(score, 0.0, "free ability should have zero scarcity");
    }

    #[test]
    fn scarcity_penalizes_expensive_on_dying_target() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let mut enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        enemy.hp = 1;
        enemy.max_hp = 20;

        let s = snap(vec![active.clone(), enemy.clone()]);
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&content, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert!(
            score < 0.0,
            "expensive ability on dying target should get negative scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_kill_on_support() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let mut enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        enemy.role = AxisProfile::from(AiRole::Support);
        enemy.hp = 5;
        enemy.max_hp = 20;

        let s = snap(vec![active.clone(), enemy.clone()]);
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&content, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);
        let score = compute_scarcity(&c, &active, 1.0, &ctx, &s);
        assert!(
            score > 0.0,
            "kill on support should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_aoe_on_cluster() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((20, 20));

        let center = hex_from_offset(2, 3);
        let neighbors: Vec<Hex> = center.all_neighbors().to_vec();
        let e1 = unit(1, Team::Player, center);
        let e2 = unit(2, Team::Player, neighbors[0]);
        let e3 = unit(3, Team::Player, neighbors[1]);

        let s = BattleSnapshot {
            units: vec![active.clone(), e1.clone(), e2.clone(), e3.clone()],
            active_unit: active.entity,
            round: 3,
        };
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = scarcity_ctx(&content, &diff, &abilities);

        let c = cast(tile, "fireball", e1.pos, e1.entity);
        let score = compute_scarcity(&c, &active, 0.0, &ctx, &s);
        assert!(
            score > 0.0,
            "AoE on cluster should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_penalizes_early_round_spend() {
        let tile = hex_from_offset(4, 3);
        let mut active = unit(0, Team::Enemy, tile);
        active.mana = Some((10, 10));

        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));

        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let abilities = Abilities(vec!["fireball".into()]);
        let ctx = scarcity_ctx(&content, &diff, &abilities);

        let c = cast(tile, "fireball", enemy.pos, enemy.entity);

        let s_r1 = BattleSnapshot {
            units: vec![active.clone(), enemy.clone()],
            active_unit: active.entity,
            round: 1,
        };
        let score_r1 = compute_scarcity(&c, &active, 0.0, &ctx, &s_r1);

        let s_r3 = BattleSnapshot {
            units: vec![active.clone(), enemy.clone()],
            active_unit: active.entity,
            round: 3,
        };
        let score_r3 = compute_scarcity(&c, &active, 0.0, &ctx, &s_r3);

        assert!(
            score_r1 < score_r3,
            "round 1 ({:.2}) should have lower scarcity than round 3 ({:.2})",
            score_r1, score_r3,
        );
    }

    // ── Normalization tests ───────────────────────────────────────────

    #[test]
    fn signed_normalization_preserves_negative_order() {
        let values = [-3.0f32, -1.0, -0.5];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let normalized: Vec<f32> = values.iter().map(|v| v / max_abs).collect();
        assert_eq!(normalized, vec![-1.0, -1.0 / 3.0, -0.5 / 3.0]);
        assert!(normalized[0] < normalized[1]);
        assert!(normalized[1] < normalized[2]);
    }

    #[test]
    fn signed_normalization_flat_batch_gives_zero() {
        let values = [0.0f32; 3];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        for &v in &values {
            let norm = if max_abs > f32::EPSILON { v / max_abs } else { 0.0 };
            assert_eq!(norm, 0.0);
            assert!(!norm.is_nan());
        }
    }
}
