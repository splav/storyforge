//! Offensive factors: damage / heal / kill / cc for single-target and AoE.
//!
//! `compute_offensive` is a pure outcome-facts reader + policy applier. AoE
//! damage walks `outcome.enemy_damage_per_entity` per-entity; friendly-fire
//! reads `ally_damage_per_entity` / `self_damage` directly.

use super::{crit_fail_adjusted, OffensiveFactors};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::content::abilities::{AbilityDef, EffectDef};
use crate::game::hex::Hex;
use bevy::prelude::*;
use combat_engine::aoe_cells;
use combat_engine::AbilityId;
use std::collections::HashSet;

/// Offensive factors for a single Cast step. All damage/heal/CC values come
/// from `outcome` + the policy module — never re-derived from the snapshot.
pub(crate) fn compute_offensive(
    ability: &AbilityId,
    _target_pos: Hex,
    target: Entity,
    _caster_tile: Hex,
    ctx: &ScoringCtx,
    outcome: &ActionOutcomeEstimate,
) -> OffensiveFactors {
    use crate::combat::ai::scoring::policy;

    let content = ctx.world.content;
    let Some(def) = content.abilities.get(ability) else {
        return OffensiveFactors::default();
    };

    if matches!(def.effect, EffectDef::Summon { .. }) {
        return OffensiveFactors::default();
    }

    let snap = ctx.snap;
    let active = ctx.active;

    // ── Damage: facts → policies ──
    let enemy_damage_value = if outcome.enemy_damage_per_entity.is_empty() {
        // Single-target path.
        snap.unit(target).map_or(0.0, |t| {
            let damage_progress = (outcome.enemy_damage / t.hp().max(1) as f32).min(1.0);
            policy::damage::value(outcome.enemy_damage, damage_progress)
        })
    } else {
        // AoE: per-entity policy application — captures per-target progression.
        outcome
            .enemy_damage_per_entity
            .iter()
            .map(|(e, dmg)| {
                snap.unit(*e).map_or(0.0, |t| {
                    let damage_progress = (*dmg / t.hp().max(1) as f32).min(1.0);
                    policy::damage::value(*dmg, damage_progress)
                })
            })
            .sum()
    };

    let ally_penalty: f32 = outcome
        .ally_damage_per_entity
        .iter()
        .map(|(e, dmg)| {
            snap.unit(*e)
                .map_or(0.0, |t| policy::friendly_fire::penalty(*dmg, t.max_hp()))
        })
        .sum();

    let self_penalty = if outcome.self_damage > 0.0 {
        policy::friendly_fire::penalty(outcome.self_damage, active.max_hp())
    } else {
        0.0
    };

    let damage_raw = enemy_damage_value - ally_penalty - self_penalty;
    let damage = crit_fail_adjusted(
        damage_raw,
        def,
        &active.cache.crit_fail_effect,
        ctx.world.crit_fail_chance,
    );

    // ── Heal: facts → policy ──
    let heal = if outcome.hp_restored > 0.0 {
        snap.unit(target).map_or(0.0, |t| {
            let danger = ctx.maps.danger.get(t.pos);
            let horizon_sum: f32 = t
                .cache
                .damage_horizon
                .iter()
                .sum::<f32>()
                .max(t.cache.threat);
            let raw =
                policy::heal::value(outcome.hp_restored, t.max_hp(), t.hp(), danger, horizon_sum);
            crit_fail_adjusted(
                raw,
                def,
                &active.cache.crit_fail_effect,
                ctx.world.crit_fail_chance,
            )
        })
    } else {
        0.0
    };

    // ── CC: facts → policy ──
    let cc = policy::cc::value(outcome.cc_turns_applied, outcome.armor_shred_applied);

    // ── Kill signals: pure facts ──
    let kill_now = outcome.p_kill_now;
    let kill_promised = outcome.p_kill_soon;

    OffensiveFactors {
        damage,
        heal,
        kill_now,
        kill_promised,
        cc,
    }
}

/// Expand an AoE def into the set of affected tiles. Thin wrapper over
/// `combat_engine::aoe_cells` that materialises the result as a `HashSet` for
/// fast `contains` checks in the planner.
pub fn aoe_area(def: &AbilityDef, target_pos: Hex, caster_tile: Hex) -> HashSet<Hex> {
    aoe_cells(def.aoe, caster_tile, target_pos)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::UnitBuilder;

    use crate::combat::ai::outcome::ActionOutcomeEstimate;
    use crate::combat::ai::scoring::policy;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::AbilityId;

    fn db() -> crate::content::content_view::ActiveContentData {
        crate::content::content_view::ActiveContentData::load_global_for_tests()
    }

    #[test]
    fn compute_offensive_reads_facts_and_applies_policy() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        use crate::combat::ai::world::reservations::Reservations;

        use crate::combat::ai::test_helpers::snapshot_from;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};

        let content = db();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);

        // Target at half HP so damage progression > 0.
        let actor = UnitBuilder::new(1, Team::Enemy, caster_pos)
            .full_hp(100)
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(50)
            .max_hp(100)
            .threat(20.0)
            .build();
        let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // Synth outcome with all relevant fact fields set.
        let outcome = ActionOutcomeEstimate {
            enemy_damage: 30.0,
            p_kill_now: 0.0,
            p_kill_soon: 1.0,
            cc_turns_applied: 5.0,
            armor_shred_applied: 1.0,
            hp_restored: 0.0,
            self_damage: 0.0,
            ..Default::default()
        };

        let ability = AbilityId::from("melee_attack");
        let off = compute_offensive(
            &ability,
            target_pos,
            target.entity,
            caster_pos,
            &ctx,
            &outcome,
        );

        // Damage: single-target path; hp_pct = min(30/50, 1.0) = 0.6.
        let expected_dmg = policy::damage::value(30.0, 0.6);
        assert!(
            (off.damage - expected_dmg).abs() < 1e-5,
            "damage={} expected≈{expected_dmg}",
            off.damage
        );

        // CC: policy::cc::value(5.0, 1.0) = 6.0.
        let expected_cc = policy::cc::value(5.0, 1.0);
        assert!(
            (off.cc - expected_cc).abs() < 1e-5,
            "cc={} expected={expected_cc}",
            off.cc
        );

        assert_eq!(off.kill_promised, 1.0, "kill_promised from p_kill_soon");
        assert_eq!(off.kill_now, 0.0, "kill_now from p_kill_now");
        assert_eq!(off.heal, 0.0, "no heal when hp_restored == 0");
    }

    /// AoE per-entity progression: damage policy applied per-target, not once
    /// on the total. A high-HP target with the same raw damage is worth less
    /// than an equivalent hit on a low-HP target.
    #[test]
    fn compute_offensive_aoe_per_entity_progression() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        use crate::combat::ai::world::reservations::Reservations;

        use crate::combat::ai::test_helpers::snapshot_from;
        use crate::combat::ai::test_helpers::{empty_maps, ent, make_scoring_ctx, make_test_ctx};

        let content = db();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, caster_pos)
            .full_hp(100)
            .build();
        // low-HP target: 10 HP remaining (raw=10 hits hard — progress=1.0).
        let low_hp = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(10)
            .max_hp(100)
            .build();
        // high-HP target: 100 HP remaining (raw=10 is minor — progress=0.1).
        let high_hp = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0))
            .full_hp(100)
            .build();

        let snap = snapshot_from(vec![actor.clone(), low_hp.clone(), high_hp.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // AoE outcome: same raw damage (10) to both targets.
        let raw = 10.0_f32;
        let outcome_aoe = ActionOutcomeEstimate {
            enemy_damage_per_entity: vec![(low_hp.entity, raw), (high_hp.entity, raw)],
            enemy_damage: raw * 2.0,
            ..Default::default()
        };

        // Single-target equivalent outcome hitting only the high-HP target.
        let outcome_single_high = ActionOutcomeEstimate {
            enemy_damage: raw,
            ..Default::default()
        };

        let ability = AbilityId::from("melee_attack");
        let off_aoe = compute_offensive(
            &ability,
            target_pos,
            low_hp.entity,
            caster_pos,
            &ctx,
            &outcome_aoe,
        );
        let off_single_high = compute_offensive(
            &ability,
            target_pos,
            high_hp.entity,
            caster_pos,
            &ctx,
            &outcome_single_high,
        );

        // The AoE value must exceed "same damage against the high-HP target only"
        // because the low-HP hit is valued much higher (kills progression).
        assert!(
            off_aoe.damage > off_single_high.damage * 2.0,
            "AoE with one near-death target ({}) should beat 2× single high-HP value ({})",
            off_aoe.damage,
            off_single_high.damage * 2.0
        );

        // Verify the low-HP progression is higher than high-HP progression.
        let val_low = policy::damage::value(raw, (raw / 10.0_f32).min(1.0));
        let val_high = policy::damage::value(raw, (raw / 100.0_f32).min(1.0));
        assert!(
            val_low > val_high,
            "low-HP target value ({val_low}) > high-HP target value ({val_high})"
        );

        // Suppress unused-import warning for ent helper used in sibling tests.
        let _ = ent(99);
    }

    /// Friendly-fire penalty is super-linear in raw_dmg/max_hp: doubling the
    /// damage dealt to an ally produces more than double the penalty.
    #[test]
    fn compute_offensive_friendly_fire_super_linear() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        use crate::combat::ai::world::reservations::Reservations;

        use crate::combat::ai::test_helpers::snapshot_from;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};

        let content = db();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, caster_pos)
            .full_hp(100)
            .build();
        // Enemy target so we don't accidentally zero-out from missing snap.unit.
        let enemy_target = UnitBuilder::new(2, Team::Player, target_pos)
            .full_hp(100)
            .build();
        // Ally that takes friendly-fire splash.
        let ally = UnitBuilder::new(3, Team::Enemy, hex_from_offset(0, 1))
            .full_hp(100)
            .build();

        let snap = snapshot_from(vec![actor.clone(), enemy_target.clone(), ally.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let ability = AbilityId::from("melee_attack");

        // Low ally damage.
        let outcome_low = ActionOutcomeEstimate {
            enemy_damage: 50.0,
            ally_damage_per_entity: vec![(ally.entity, 10.0)],
            ally_damage: 10.0,
            ..Default::default()
        };
        // Double ally damage.
        let outcome_high = ActionOutcomeEstimate {
            enemy_damage: 50.0,
            ally_damage_per_entity: vec![(ally.entity, 20.0)],
            ally_damage: 20.0,
            ..Default::default()
        };

        let off_low = compute_offensive(
            &ability,
            target_pos,
            enemy_target.entity,
            caster_pos,
            &ctx,
            &outcome_low,
        );
        let off_high = compute_offensive(
            &ability,
            target_pos,
            enemy_target.entity,
            caster_pos,
            &ctx,
            &outcome_high,
        );

        // The plan with higher ally damage should have lower net damage
        // (super-linear penalty formula is covered by friendly_fire.rs::super_linear_growth).
        assert!(
            off_high.damage < off_low.damage,
            "higher ally damage ({}) should reduce net damage below low ally case ({})",
            off_high.damage,
            off_low.damage
        );
    }
}
