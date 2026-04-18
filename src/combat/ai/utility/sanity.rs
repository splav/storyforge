//! Post-score sanity penalties on top candidates + defensive classification.

#![allow(clippy::too_many_arguments)]

use super::UtilityContext;
use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::factors::aoe_area;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ContentView;
use crate::game::hex::{has_los, in_bounds, Hex};
use std::collections::HashSet;

/// Post-score verification on the top-3 candidates. Applies multiplicative
/// penalties for dangerous situations that per-factor scoring can't catch.
pub(super) fn sanity_adjust(
    scores: &mut [f32],
    candidates: &[ActionCandidate],
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    ctx: &UtilityContext,
) {
    if scores.len() <= 1 {
        return;
    }

    // Find top-3 indices by score.
    let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(3);

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap.allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    let occupied: HashSet<Hex> = snap.units.iter().map(|u| u.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, maps);
    let current_danger = maps.danger.get(active.pos);

    for (idx, _) in &indexed {
        let c = &candidates[*idx];
        let mut penalty = 1.0f32;

        // 1. Survival: quadratic penalty on (low HP × dangerous tile).
        // Replaces the old step penalty (×0.3 / ×0.6) and the constraint-level
        // "don't walk into death" hard filter — hard cuts left the AI with no
        // retreat option when every reachable tile was dangerous. Gradient
        // lets retreat candidates reach scoring and compete; heal usually
        // still wins when available, retreat wins when nothing else does.
        //
        //   penalty_frac = LOW_HP_FACTOR × hp_need × max(0, danger − 0.5)²
        //   hp_need      = clamp((0.6 − hp_pct) / 0.6, 0, 1)
        //   score        *= (1 − penalty_frac).max(0.25)  // floor to stay comparable
        const LOW_HP_FACTOR: f32 = 1.2;
        let danger_frac = maps.danger.get(c.tile);
        let hp_fraction = active.hp_pct();
        let hp_need = ((0.6 - hp_fraction) / 0.6).clamp(0.0, 1.0);
        let excess = (danger_frac - 0.5).max(0.0);
        let penalty_frac = LOW_HP_FACTOR * hp_need * excess * excess;
        if penalty_frac > 0.0 {
            penalty *= (1.0 - penalty_frac).max(0.25);
        }

        // 2. Healer exposure: are we abandoning an allied healer?
        // Healer exposure check: if the actor isn't itself a significant
        // healer (Support axis < 0.3), it shouldn't abandon the team healer.
        if active.role.support < 0.3 {
            for ally in &allies {
                if !ally.tags.contains(AiTags::CAN_HEAL) {
                    continue;
                }
                let was_near = active.pos.unsigned_distance_to(ally.pos) <= 1;
                let will_be_far = c.tile.unsigned_distance_to(ally.pos) > 2;
                if was_near && will_be_far {
                    let other_guard = allies.iter().any(|a| {
                        a.entity != ally.entity && a.pos.unsigned_distance_to(ally.pos) <= 2
                    });
                    if !other_guard {
                        penalty *= 0.5;
                    }
                }
            }
        }

        // 3. LOS check: ranged unit moving to a blind spot.
        if active.tags.contains(AiTags::RANGED) && !enemies.is_empty() {
            let can_see_any = enemies.iter().any(|e| {
                has_los(c.tile, e.pos, |mid| {
                    occupied.contains(&mid) && mid != c.tile && mid != e.pos
                })
            });
            if !can_see_any {
                penalty *= 0.3;
            }
        }

        // 4. Retreat trap: tile with very few unblocked neighbors.
        let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
        let open_neighbors = c.tile.all_neighbors().iter()
            .filter(|&&n| in_bounds(n) && !ally_positions.contains(&n))
            .count();
        if open_neighbors < 2 {
            penalty *= 0.5;
        }

        // 5. Self-AoE: heavy penalty for friendly_fire AoE that hits caster.
        if let CandidateKind::Cast { ability, target_pos, .. } = &c.kind {
            if let Some(def) = ctx.content.abilities.get(ability) {
                if def.friendly_fire && def.aoe != AoEShape::None {
                    let area = aoe_area(def, *target_pos, c.tile);
                    if area.contains(&c.tile) {
                        penalty *= 0.5;
                    }
                }
            }
        }

        // 6. Synergy bonus: candidate that MOVES to a better tile AND casts a
        // useful ability — the "retreat-and-help" combo. Multiplicative so it
        // doesn't flip sign and scales with base score magnitude.
        if c.tile != active.pos {
            let safer_tile = maps.danger.get(c.tile) + 0.05 < current_danger;
            let better_pos = evaluate_position(c.tile, &active.role, maps) > current_pos_eval;
            let useful_cast = match &c.kind {
                CandidateKind::Cast { ability, .. } => {
                    ctx.content.abilities.get(ability).is_some_and(|def| {
                        def.effect.calc(ctx.caster).is_some() || !def.statuses.is_empty()
                    })
                }
                CandidateKind::MoveOnly => false,
            };
            if (safer_tile || better_pos) && useful_cast {
                penalty *= 1.1;
            }
        }

        scores[*idx] *= penalty;
    }
}

/// A candidate is defensive if it heals/buffs self/ally, is pure movement to a
/// safer tile, OR an offensive action from a safer tile.
pub(super) fn is_defensive(
    c: &ActionCandidate,
    current_danger: f32,
    content: &ContentView,
    maps: &InfluenceMaps,
    margin: f32,
) -> bool {
    // MoveOnly is defensive when moving to a safer tile.
    if c.is_move_only() {
        return maps.danger.get(c.tile) + margin < current_danger;
    }
    if let Some(ability) = c.ability() {
        if let Some(def) = content.abilities.get(ability) {
            if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                return true;
            }
        }
    }
    // Cast from a meaningfully safer tile also counts as defensive.
    maps.danger.get(c.tile) + margin < current_danger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::CasterContext;
    use crate::content::content_view::ContentView;
    use crate::content::races::CritFailEffect;
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::hex_from_offset;
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
            action: true,
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
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        let active = units[0].entity;
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    fn empty_maps() -> InfluenceMaps {
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
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

    #[test]
    fn sanity_penalizes_suicide_tile() {
        let dangerous = hex_from_offset(3, 3);
        let safe_tile = hex_from_offset(5, 4);
        let mut active = unit(0, Team::Enemy, hex_from_offset(4, 3));
        active.hp = 5; // low HP so survival check triggers
        let enemy = unit(1, Team::Player, hex_from_offset(2, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);

        let mut maps = empty_maps();
        // Normalized danger: 0.9 = very dangerous, 0.1 = safe.
        maps.danger.add(dangerous, 0.9);
        maps.danger.add(safe_tile, 0.1);

        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { content: &content, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(dangerous, enemy.entity),
            candidate(safe_tile, enemy.entity),
        ];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        assert!(
            scores[0] < scores[1],
            "dangerous tile ({:.1}) should score lower than safe ({:.1}) after sanity",
            scores[0], scores[1],
        );
    }

    #[test]
    fn sanity_preserves_safe_candidate() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(2, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);

        let maps = empty_maps(); // no danger anywhere
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { content: &content, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(tile, enemy.entity),
            candidate(hex_from_offset(3, 3), enemy.entity),
        ];
        let mut scores = vec![10.0, 8.0];
        let original = scores.clone();

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        // First candidate (safe tile, no danger) should keep full score.
        assert_eq!(scores[0], original[0], "safe candidate score should be unchanged");
    }

    #[test]
    fn sanity_ranged_penalizes_blind_spot() {
        let actor_pos = hex_from_offset(4, 3);
        let behind_wall = hex_from_offset(0, 0);
        let mut active = unit(0, Team::Enemy, actor_pos);
        active.tags = AiTags::RANGED;
        let enemy = unit(1, Team::Player, hex_from_offset(4, 1));

        // Place a blocker between (0,0) and (4,1) — any unit on the line.
        let blocker = unit(2, Team::Enemy, hex_from_offset(2, 1));
        let s = snap(vec![active.clone(), enemy.clone(), blocker]);

        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = UtilityContext { content: &content, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        let candidates = vec![
            candidate(behind_wall, enemy.entity),
            candidate(actor_pos, enemy.entity), // stay — has LOS
        ];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        // The blind-spot tile should be penalized.
        assert!(
            scores[0] < 10.0,
            "blind-spot tile should be penalized, got {:.1}",
            scores[0],
        );
    }

    #[test]
    fn sanity_penalizes_self_aoe() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(4, 2));
        let s = snap(vec![active.clone(), enemy.clone()]);
        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let caster = CasterContext { str_mod: 0, int_mod: 3, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec!["fireball".into(), "melee_attack".into()]);
        let ctx = UtilityContext { content: &content, difficulty: &diff, caster: &caster, abilities: &abilities, opponent_team: Team::Player, crit_fail_effect: CritFailEffect::Miss, crit_fail_chance: 0.0 };

        // thunderstrike AoE circle r=1 centered on caster's own tile → self-hit.
        let self_aoe = cast(tile, "thunderstrike", tile, enemy.entity);
        let safe = candidate(tile, enemy.entity); // melee_attack, no AoE

        let candidates = vec![self_aoe, safe];
        let mut scores = vec![10.0, 9.0];

        sanity_adjust(&mut scores, &candidates, &active, &s, &maps, &ctx);

        assert!(
            scores[0] < 10.0,
            "self-AoE should be penalized, got {:.1}",
            scores[0],
        );
        assert!(
            scores[0] < scores[1],
            "self-AoE ({:.1}) should score lower than safe ({:.1})",
            scores[0], scores[1],
        );
    }
}
