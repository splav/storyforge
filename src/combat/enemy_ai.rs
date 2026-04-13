use crate::content::abilities::{AbilityDef, EffectDef, TargetType};
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{
    Abilities, ActionPoints, Combatant, Faction, Mana, Rage, Speed, StatusEffects, Team, Vital,
};
use crate::game::hex::{hex_distance, in_bounds};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::{find_path, reachable_cells};
use crate::game::resources::{CombatContext, GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::HashSet;

/// Automatically acts on behalf of enemy-controlled combatants.
/// Picks the best affordable ability, prefers healing wounded allies,
/// then strongest ranged/melee attacks. Moves toward targets when out of range.
pub fn enemy_ai_system(
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    mut rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    mut move_unit: MessageWriter<MoveUnit>,
    mut end_turn: MessageWriter<EndTurn>,
    combatants: Query<
        (
            Entity,
            &Faction,
            &Abilities,
            &Vital,
            &Speed,
            &ActionPoints,
            Option<&Mana>,
            Option<&Rage>,
        ),
        With<Combatant>,
    >,
    statuses: Query<&StatusEffects>,
) {
    let Some(actor) = ctx.active else { return };
    let Ok((_, faction, abilities, vital, speed, ap, mana, rage)) = combatants.get(actor) else {
        return;
    };
    if faction.0 != Team::Enemy || !vital.is_alive() || abilities.0.is_empty() {
        return;
    }
    if !ap.action && !ap.movement {
        return;
    }

    let Some(&actor_pos) = positions.0.get(&actor) else {
        return;
    };

    let mana_cur = mana.map(|m| m.current).unwrap_or(0);
    let rage_cur = rage.map(|r| r.current).unwrap_or(0);

    // Collect living players with positions.
    let players: Vec<(Entity, (i32, i32))> = combatants
        .iter()
        .filter(|(_, f, _, v, _, _, _, _)| f.0 == Team::Player && v.is_alive())
        .filter_map(|(e, _, _, _, _, _, _, _)| positions.0.get(&e).map(|&p| (e, p)))
        .collect();

    if players.is_empty() {
        return;
    }

    // Forced targeting (taunt).
    let forced: Vec<(Entity, (i32, i32))> = players
        .iter()
        .copied()
        .filter(|(e, _)| {
            statuses.get(*e).map_or(false, |se| {
                se.0.iter().any(|s| {
                    db.statuses
                        .get(&s.id)
                        .map_or(false, |def| def.forces_targeting)
                })
            })
        })
        .collect();
    let enemy_pool = if forced.is_empty() {
        &players
    } else {
        &forced
    };

    // Collect living allies (same team, for healing).
    let allies: Vec<(Entity, (i32, i32), i32, i32)> = combatants
        .iter()
        .filter(|(_, f, _, v, _, _, _, _)| f.0 == Team::Enemy && v.is_alive())
        .filter_map(|(e, _, _, v, _, _, _, _)| {
            positions.0.get(&e).map(|&p| (e, p, v.hp, v.max_hp))
        })
        .collect();

    // Score all (ability, target) pairs.
    let mut best_in_range: Option<(AbilityId, Entity, i32)> = None; // (ability, target, score)
    let mut best_any: Option<(AbilityId, Entity, i32, i32)> = None; // + range needed

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else {
            continue;
        };
        if !can_afford(def, mana_cur, rage_cur) {
            continue;
        }

        let candidates = match def.target_type {
            TargetType::SingleEnemy => {
                enemy_pool
                    .iter()
                    .map(|&(e, pos)| (e, pos))
                    .collect::<Vec<_>>()
            }
            TargetType::SingleAlly => {
                allies.iter().map(|&(e, pos, _, _)| (e, pos)).collect()
            }
            TargetType::Myself => vec![(actor, actor_pos)],
        };

        for (target, target_pos) in candidates {
            let score = score_ability(def, target, &allies, &db);
            if score <= 0 {
                continue;
            }

            let dist = hex_distance(actor_pos.0, actor_pos.1, target_pos.0, target_pos.1);
            let range = def.range as i32;

            // Track best overall (for movement planning).
            if best_any.as_ref().map_or(true, |b| score > b.2) {
                best_any = Some((ability_id.clone(), target, score, range));
            }

            // Track best in range from current position.
            if dist <= range || range == 0 {
                if best_in_range.as_ref().map_or(true, |b| score > b.2) {
                    best_in_range = Some((ability_id.clone(), target, score));
                }
            }
        }
    }

    // If we have an ability+target in range, use it.
    if let Some((ability, target, _)) = best_in_range {
        if ap.action {
            use_ability.write(UseAbility {
                actor,
                ability,
                target,
            });
            return;
        }
    }

    // Not in range. Try to move closer for the best ability.
    if !ap.movement {
        end_turn.write(EndTurn { actor });
        return;
    }

    let target_range = best_any.as_ref().map(|b| b.3).unwrap_or(1);

    // Build passability sets.
    let enemy_positions: HashSet<(i32, i32)> = combatants
        .iter()
        .filter(|(e, f, _, v, _, _, _, _)| *e != actor && f.0 == Team::Enemy && v.is_alive())
        .filter_map(|(e, _, _, _, _, _, _, _)| positions.0.get(&e).copied())
        .collect();
    let all_occupied: HashSet<(i32, i32)> = positions
        .0
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();

    let is_passable =
        |q: i32, r: i32| -> bool { in_bounds(q, r) && !enemy_positions.contains(&(q, r)) };

    let reachable = reachable_cells(
        actor_pos,
        speed.0,
        &is_passable,
        |q, r| !all_occupied.contains(&(q, r)),
    );

    // Find best reachable cell that puts us within ability range of a target.
    let move_targets: Vec<(Entity, (i32, i32))> = if let Some((_, target, _, _)) = &best_any {
        // Try to reach the specific best target.
        if let Some(&pos) = positions.0.get(target) {
            vec![(*target, pos)]
        } else {
            enemy_pool.to_vec()
        }
    } else {
        enemy_pool.to_vec()
    };

    let mut best_move: Option<(Vec<(i32, i32)>, Entity)> = None;

    for &(target_entity, target_pos) in &move_targets {
        for &cell in &reachable {
            if hex_distance(cell.0, cell.1, target_pos.0, target_pos.1) > target_range {
                continue;
            }
            if let Some(path) = find_path(actor_pos, cell, &is_passable) {
                if path.len() as i32 <= speed.0 {
                    let is_better = best_move
                        .as_ref()
                        .map_or(true, |(bp, _)| path.len() < bp.len());
                    if is_better {
                        best_move = Some((path, target_entity));
                    }
                }
            }
        }
    }

    if let Some((path, target)) = best_move {
        let dest = *path.last().unwrap();
        move_unit.write(MoveUnit {
            actor,
            path,
        });
        if ap.action {
            // Re-pick best ability for this target from new position.
            let best_ability = pick_best_for_target(
                actor, target, dest, abilities, &db, mana_cur, rage_cur, &allies,
            );
            if let Some(ability) = best_ability {
                use_ability.write(UseAbility {
                    actor,
                    ability,
                    target,
                });
            }
        }
        return;
    }

    // Can't reach attack range — move as close as possible.
    let mut best_approach: Option<((i32, i32), i32)> = None;
    for &cell in &reachable {
        for &(_, target_pos) in enemy_pool {
            let dist = hex_distance(cell.0, cell.1, target_pos.0, target_pos.1);
            if best_approach.map_or(true, |(_, bd)| dist < bd) {
                best_approach = Some((cell, dist));
            }
        }
    }

    if let Some((dest, _)) = best_approach {
        if let Some(path) = find_path(actor_pos, dest, &is_passable) {
            if path.len() as i32 <= speed.0 {
                move_unit.write(MoveUnit { actor, path });
            }
        }
    }

    end_turn.write(EndTurn { actor });
}

fn can_afford(def: &AbilityDef, mana: i32, rage: i32) -> bool {
    if def.mana_cost > 0 && mana < def.mana_cost {
        return false;
    }
    if def.rage_cost > 0 && rage < def.rage_cost {
        return false;
    }
    true
}

/// Score an ability for a given target. Higher = better. 0 = skip.
fn score_ability(
    def: &AbilityDef,
    target: Entity,
    allies: &[(Entity, (i32, i32), i32, i32)], // (entity, pos, hp, max_hp)
    db: &GameDb,
) -> i32 {
    match &def.effect {
        EffectDef::Heal { dice } => {
            // Only heal if target is below 60% HP.
            let (hp, max_hp) = allies
                .iter()
                .find(|(e, _, _, _)| *e == target)
                .map(|(_, _, hp, max_hp)| (*hp, *max_hp))
                .unwrap_or((1, 1));
            if max_hp == 0 || hp * 100 / max_hp > 60 {
                return 0; // Not worth healing.
            }
            let missing = max_hp - hp;
            let avg_heal = (dice.count * dice.sides / 2) as i32;
            // Higher priority the more HP is missing.
            missing.min(avg_heal) * 10 + 50
        }
        EffectDef::SpellDamage { dice } => {
            // Pierces armor — very valuable.
            (dice.count * dice.sides) as i32 + 20
        }
        EffectDef::Damage { dice } => (dice.count * dice.sides) as i32 + 5,
        EffectDef::WeaponAttack => 8, // Base melee, always works.
        EffectDef::None => {
            // Status-only ability. Score based on status effects.
            let mut s = 0i32;
            for sa in &def.statuses {
                if let Some(sd) = db.statuses.get(&sa.status) {
                    if sd.skips_turn {
                        s += 40; // Stun/paralyze is very valuable.
                    }
                    if sd.damage_taken_bonus > 0 {
                        s += 15;
                    }
                }
            }
            s
        }
        EffectDef::GrantMovement { .. } => 0, // Enemies don't need this.
    }
}

/// Pick the best affordable ability for a specific target from a given position.
fn pick_best_for_target(
    actor: Entity,
    target: Entity,
    from: (i32, i32),
    abilities: &Abilities,
    db: &GameDb,
    mana: i32,
    rage: i32,
    allies: &[(Entity, (i32, i32), i32, i32)],
) -> Option<AbilityId> {
    let target_pos = allies
        .iter()
        .find(|(e, _, _, _)| *e == target)
        .map(|(_, pos, _, _)| *pos);
    // For enemy targets we don't have them in allies, estimate distance from position.

    let mut best: Option<(AbilityId, i32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else {
            continue;
        };
        if !can_afford(def, mana, rage) {
            continue;
        }
        // Check range from the destination cell.
        if def.range > 0 {
            if let Some(tp) = target_pos {
                if hex_distance(from.0, from.1, tp.0, tp.1) > def.range as i32 {
                    continue;
                }
            }
        }
        let score = score_ability(def, target, allies, db);
        if score > 0 && best.as_ref().map_or(true, |b| score > b.1) {
            best = Some((ability_id.clone(), score));
        }
    }

    best.map(|(id, _)| id)
}
