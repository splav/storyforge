use crate::core::DiceRng;
use crate::game::components::{
    Abilities, ActionPoints, Combatant, Faction, Speed, StatusEffects, Team, Vital,
};
use crate::game::hex::{hex_distance, in_bounds};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::{find_path, reachable_cells};
use crate::game::resources::{CombatContext, GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::HashSet;

/// Automatically acts on behalf of enemy-controlled combatants.
/// Moves toward heroes when out of range, then attacks if possible.
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
        ),
        With<Combatant>,
    >,
    statuses: Query<&StatusEffects>,
) {
    let Some(actor) = ctx.active else { return };
    let Ok((_, faction, abilities, vital, speed, ap)) = combatants.get(actor) else {
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

    // Collect living players with positions.
    let players: Vec<(Entity, (i32, i32))> = combatants
        .iter()
        .filter(|(_, f, _, v, _, _)| f.0 == Team::Player && v.is_alive())
        .filter_map(|(e, _, _, _, _, _)| positions.0.get(&e).map(|&p| (e, p)))
        .collect();

    if players.is_empty() {
        return;
    }

    // If any living player has a forces_targeting status, enemies must target them.
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

    let pool = if forced.is_empty() {
        &players
    } else {
        &forced
    };

    // Pick a random ability.
    let ability_idx = rng.roll_d(abilities.0.len() as u32) as usize - 1;
    let ability = abilities.0[ability_idx].clone();
    let range = db
        .abilities
        .get(&ability)
        .map(|d| d.range as i32)
        .unwrap_or(1);

    // Check if any target in pool is within range from current position.
    let in_range_targets: Vec<Entity> = pool
        .iter()
        .filter(|(_, pos)| hex_distance(actor_pos.0, actor_pos.1, pos.0, pos.1) <= range)
        .map(|(e, _)| *e)
        .collect();

    if !in_range_targets.is_empty() && ap.action {
        // Attack directly.
        let idx = rng.roll_d(in_range_targets.len() as u32) as usize - 1;
        use_ability.write(UseAbility {
            actor,
            ability,
            target: in_range_targets[idx],
        });
        return;
    }

    // Not in range (or can't act). Try to move closer.
    if !ap.movement {
        // Can't move and nobody in range — end turn.
        if ap.action {
            // Has action but nobody in range — still end turn.
            end_turn.write(EndTurn { actor });
        }
        return;
    }

    // Build passability sets.
    let enemy_positions: HashSet<(i32, i32)> = combatants
        .iter()
        .filter(|(e, f, _, v, _, _)| *e != actor && f.0 == Team::Enemy && v.is_alive())
        .filter_map(|(e, _, _, _, _, _)| positions.0.get(&e).copied())
        .collect();
    let all_occupied: HashSet<(i32, i32)> = positions
        .0
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();

    let is_passable = |q: i32, r: i32| -> bool {
        in_bounds(q, r) && !enemy_positions.contains(&(q, r))
    };

    let reachable = reachable_cells(
        actor_pos,
        speed.0,
        &is_passable,
        |q, r| !all_occupied.contains(&(q, r)),
    );

    // Find best reachable cell that puts us within attack range of a target.
    let mut best_move: Option<(Vec<(i32, i32)>, Entity)> = None;

    for &(target_entity, target_pos) in pool {
        // Check reachable cells within range of this target.
        for &cell in &reachable {
            if hex_distance(cell.0, cell.1, target_pos.0, target_pos.1) > range {
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
        move_unit.write(MoveUnit {
            actor,
            path: path.clone(),
        });
        if ap.action {
            use_ability.write(UseAbility {
                actor,
                ability,
                target,
            });
        }
        return;
    }

    // Can't reach attack range — move as close as possible to any target.
    let mut best_approach: Option<((i32, i32), i32)> = None;
    for &cell in &reachable {
        for &(_, target_pos) in pool {
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
