use crate::content::abilities::{AbilityDef, EffectDef, TargetType};
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AiCombatantQ, Combatant, Speed, StatusEffects, Team,
};
use crate::game::hex::{hex_distance, in_bounds};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{CombatContext, GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::HashSet;

// ── AI scoring weights ─────────────────────────────────────────────────────

const HEAL_THRESHOLD_PCT: i32 = 60;
const HEAL_PER_HP_MISSING: i32 = 10;
const HEAL_BASE_BONUS: i32 = 50;
const SCORE_SPELL_DAMAGE_BONUS: i32 = 20;
const SCORE_PHYSICAL_DAMAGE_BONUS: i32 = 5;
const SCORE_WEAPON_ATTACK: i32 = 8;
const SCORE_STUN_STATUS: i32 = 40;
const SCORE_VULNERABILITY_STATUS: i32 = 15;

// ── Data types ─────────────────────────────────────────────────────────────

struct EvalResult {
    best_in_range: Option<(AbilityId, Entity, i32)>,
    best_any: Option<(AbilityId, Entity, i32, i32)>, // + range needed
}

enum MoveDecision {
    MoveAndAttack {
        path: Vec<(i32, i32)>,
        ability: AbilityId,
        target: Entity,
    },
    MoveCloser {
        path: Vec<(i32, i32)>,
    },
    Stay,
}

// ── Main system ────────────────────────────────────────────────────────────

/// Automatically acts on behalf of enemy-controlled combatants.
/// Picks the best affordable ability, prefers healing wounded allies,
/// then strongest ranged/melee attacks. Moves toward targets when out of range.
pub fn enemy_ai_system(
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    _rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    mut move_unit: MessageWriter<MoveUnit>,
    mut end_turn: MessageWriter<EndTurn>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<AiCombatantQ, With<Combatant>>,
    statuses: Query<&StatusEffects>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(c) = combatants.get(actor) else {
        return;
    };
    if c.faction.0 != Team::Enemy || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    if ctx.turn_ending {
        return;
    }
    if !c.ap.action && !c.ap.movement {
        end_turn.write(EndTurn { actor });
        return;
    }

    let (abilities, ap, speed) = (c.abilities, c.ap, c.speed);
    let Some(actor_pos) = positions.get(&actor) else {
        return;
    };
    let mana_cur = c.mana.map(|m| m.current).unwrap_or(0);
    let rage_cur = c.rage.map(|r| r.current).unwrap_or(0);

    // Collect living players with positions.
    let players: Vec<(Entity, (i32, i32))> = combatants
        .iter()
        .filter(|c| c.faction.0 == Team::Player && c.vital.is_alive())
        .filter_map(|c| positions.get(&c.entity).map(|p| (c.entity, p)))
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
        .filter(|c| c.faction.0 == Team::Enemy && c.vital.is_alive())
        .filter_map(|c| {
            positions
                .get(&c.entity)
                .map(|p| (c.entity, p, c.vital.hp, c.vital.max_hp))
        })
        .collect();

    // Evaluate all (ability, target) pairs.
    let eval = evaluate_targets(actor_pos, abilities, enemy_pool, &allies, &db, mana_cur, rage_cur);

    // If we have an ability+target in range, use it.
    if let Some((ref ability, target, _)) = eval.best_in_range {
        if ap.action {
            use_ability.write(UseAbility {
                actor,
                ability: ability.clone(),
                target,
            });
            return;
        }
    }

    // Try to move closer and/or attack after moving.
    if ap.movement {
        let decision = plan_movement(
            actor,
            actor_pos,
            speed,
            abilities,
            enemy_pool,
            &allies,
            &eval,
            ap,
            &db,
            mana_cur,
            rage_cur,
            &positions,
            &combatants,
        );

        match decision {
            MoveDecision::MoveAndAttack {
                path,
                ability,
                target,
            } => {
                move_unit.write(MoveUnit { actor, path });
                use_ability.write(UseAbility {
                    actor,
                    ability,
                    target,
                });
                return;
            }
            MoveDecision::MoveCloser { path } => {
                move_unit.write(MoveUnit { actor, path });
            }
            MoveDecision::Stay => {}
        }
    }

    end_turn.write(EndTurn { actor });
}

// ── Evaluator ──────────────────────────────────────────────────────────────

/// Score all (ability, target) pairs and return the best in-range and best overall.
fn evaluate_targets(
    actor_pos: (i32, i32),
    abilities: &Abilities,
    enemy_pool: &[(Entity, (i32, i32))],
    allies: &[(Entity, (i32, i32), i32, i32)],
    db: &GameDb,
    mana_cur: i32,
    rage_cur: i32,
) -> EvalResult {
    let mut best_in_range: Option<(AbilityId, Entity, i32)> = None;
    let mut best_any: Option<(AbilityId, Entity, i32, i32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else {
            continue;
        };
        if !can_afford(def, mana_cur, rage_cur) {
            continue;
        }

        let candidates: Vec<(Entity, (i32, i32))> = match def.target_type {
            TargetType::SingleEnemy => enemy_pool.to_vec(),
            TargetType::SingleAlly => allies.iter().map(|&(e, pos, _, _)| (e, pos)).collect(),
            TargetType::Myself => vec![(Entity::PLACEHOLDER, actor_pos)],
        };

        for (target, target_pos) in candidates {
            let score = score_ability(def, target, allies, db);
            if score <= 0 {
                continue;
            }

            let dist = hex_distance(actor_pos.0, actor_pos.1, target_pos.0, target_pos.1);
            let range = def.range as i32;

            if best_any.as_ref().map_or(true, |b| score > b.2) {
                best_any = Some((ability_id.clone(), target, score, range));
            }

            if dist <= range || range == 0 {
                if best_in_range.as_ref().map_or(true, |b| score > b.2) {
                    best_in_range = Some((ability_id.clone(), target, score));
                }
            }
        }
    }

    EvalResult {
        best_in_range,
        best_any,
    }
}

// ── Movement planner ───────────────────────────────────────────────────────

/// Given evaluation results, plan the best movement: move into attack range,
/// or approach the nearest enemy if attack range is unreachable.
fn plan_movement(
    actor: Entity,
    actor_pos: (i32, i32),
    speed: &Speed,
    abilities: &Abilities,
    enemy_pool: &[(Entity, (i32, i32))],
    allies: &[(Entity, (i32, i32), i32, i32)],
    eval: &EvalResult,
    ap: &ActionPoints,
    db: &GameDb,
    mana_cur: i32,
    rage_cur: i32,
    positions: &HexPositions,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
) -> MoveDecision {
    let target_range = eval.best_any.as_ref().map(|b| b.3).unwrap_or(1);

    // Build passability sets.
    let enemy_positions: HashSet<(i32, i32)> = combatants
        .iter()
        .filter(|c| c.entity != actor && c.faction.0 == Team::Enemy && c.vital.is_alive())
        .filter_map(|c| positions.get(&c.entity))
        .collect();
    let all_occupied: HashSet<(i32, i32)> = positions
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();

    let is_passable =
        |q: i32, r: i32| -> bool { in_bounds(q, r) && !enemy_positions.contains(&(q, r)) };

    let reach = reachable_with_paths(
        actor_pos,
        speed.0,
        &is_passable,
        |q, r| !all_occupied.contains(&(q, r)),
    );

    // Find best reachable cell that puts us within ability range of any target.
    let mut best_move: Option<(Vec<(i32, i32)>, Entity)> = None;

    for &(target_entity, target_pos) in enemy_pool {
        for &cell in &reach.destinations {
            if hex_distance(cell.0, cell.1, target_pos.0, target_pos.1) > target_range {
                continue;
            }
            if let Some(path) = reach.path_to(cell) {
                let is_better = best_move
                    .as_ref()
                    .map_or(true, |(bp, _)| path.len() < bp.len());
                if is_better {
                    best_move = Some((path, target_entity));
                }
            }
        }
    }

    if let Some((path, target)) = best_move {
        let dest = *path.last().unwrap();
        if ap.action {
            let best_ability = pick_best_for_target(
                actor, target, dest, abilities, db, mana_cur, rage_cur, allies,
            );
            if let Some(ability) = best_ability {
                return MoveDecision::MoveAndAttack {
                    path,
                    ability,
                    target,
                };
            }
        }
        return MoveDecision::MoveCloser { path };
    }

    // Can't reach attack range — move as close as possible.
    let mut best_approach: Option<((i32, i32), i32)> = None;
    for &cell in &reach.destinations {
        for &(_, target_pos) in enemy_pool {
            let dist = hex_distance(cell.0, cell.1, target_pos.0, target_pos.1);
            if best_approach.map_or(true, |(_, bd)| dist < bd) {
                best_approach = Some((cell, dist));
            }
        }
    }

    if let Some((dest, _)) = best_approach {
        if let Some(path) = reach.path_to(dest) {
            return MoveDecision::MoveCloser { path };
        }
    }

    MoveDecision::Stay
}

// ── Helpers ────────────────────────────────────────────────────────────────

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
    allies: &[(Entity, (i32, i32), i32, i32)],
    db: &GameDb,
) -> i32 {
    match &def.effect {
        EffectDef::Heal { dice } => {
            let (hp, max_hp) = allies
                .iter()
                .find(|(e, _, _, _)| *e == target)
                .map(|(_, _, hp, max_hp)| (*hp, *max_hp))
                .unwrap_or((1, 1));
            if max_hp == 0 || hp * 100 / max_hp > HEAL_THRESHOLD_PCT {
                return 0;
            }
            let missing = max_hp - hp;
            let avg_heal = (dice.count * dice.sides / 2) as i32;
            missing.min(avg_heal) * HEAL_PER_HP_MISSING + HEAL_BASE_BONUS
        }
        EffectDef::SpellDamage { dice } => {
            (dice.count * dice.sides) as i32 + SCORE_SPELL_DAMAGE_BONUS
        }
        EffectDef::Damage { dice } => {
            (dice.count * dice.sides) as i32 + SCORE_PHYSICAL_DAMAGE_BONUS
        }
        EffectDef::WeaponAttack => SCORE_WEAPON_ATTACK,
        EffectDef::None => {
            let mut s = 0i32;
            for sa in &def.statuses {
                if let Some(sd) = db.statuses.get(&sa.status) {
                    if sd.skips_turn {
                        s += SCORE_STUN_STATUS;
                    }
                    if sd.damage_taken_bonus > 0 {
                        s += SCORE_VULNERABILITY_STATUS;
                    }
                }
            }
            s
        }
        EffectDef::GrantMovement { .. } => 0,
    }
}

/// Pick the best affordable ability for a specific target from a given position.
fn pick_best_for_target(
    _actor: Entity,
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

    let mut best: Option<(AbilityId, i32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else {
            continue;
        };
        if !can_afford(def, mana, rage) {
            continue;
        }
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
