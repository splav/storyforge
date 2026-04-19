//! Resource-scarcity factor: justification for spending costly abilities.

#![allow(clippy::too_many_arguments)]

use super::offensive::aoe_area;
use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::ResourceKind;

/// Compute resource-scarcity factor: `swing_value - resource_ratio`.
/// Free abilities return 0.0 (neutral). Expensive abilities on low-value
/// situations get negative scores; expensive abilities in high-swing moments
/// get positive scores.
pub(super) fn compute_scarcity(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::CasterContext;
    use crate::content::content_view::ContentView;
    use crate::content::races::CritFailEffect;
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::{hex_from_offset, Hex};
    use bevy::prelude::*;

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
            action_points: 1,
            max_ap: 1,
            movement_points: 3,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec!["melee_attack".into()],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
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
            blocked_tiles: crate::combat::ai::utility::empty_blocked_tiles(),
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
}
