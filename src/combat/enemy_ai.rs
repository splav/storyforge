use crate::combat::ai_difficulty::DifficultyProfile;
use crate::combat::ai_scoring::{score_action, estimate_threat, ActorCtx, TargetInfo};
use crate::content::abilities::{AbilityDef, TargetType};
use crate::core::{modifier, AbilityId, DiceRng};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AiCombatantQ, AiCombatantQItem,
    Combatant, Speed, StatusEffects, Team,
};
use crate::game::hex::{hex_distance, in_bounds};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::HashSet;

// ── Data types ─────────────────────────────────────────────────────────────

struct EvalResult {
    best_in_range: Option<(AbilityId, Entity, f32)>,
    best_any: Option<(AbilityId, Entity, f32, i32)>, // + ability range
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

pub fn enemy_ai_system(
    db: Res<GameDb>,
    difficulty: Res<DifficultyProfile>,
    positions: Res<HexPositions>,
    mut rng: ResMut<DiceRng>,
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
    let actor_ctx = build_actor_ctx(&c, &db);

    // Build TargetInfo for all living players.
    let all_players: Vec<TargetInfo> = combatants
        .iter()
        .filter(|t| t.faction.0 == Team::Player && t.vital.is_alive())
        .filter_map(|t| Some(build_target_info(&t, positions.get(&t.entity)?, &statuses, &db)))
        .collect();

    if all_players.is_empty() {
        return;
    }

    // Forced targeting (taunt): filter to taunting targets if any.
    let forced: Vec<&TargetInfo> = all_players
        .iter()
        .filter(|t| has_forces_targeting(t.entity, &statuses, &db))
        .collect();
    let enemy_infos: Vec<&TargetInfo> = if forced.is_empty() {
        all_players.iter().collect()
    } else {
        forced
    };

    // Living allies (same team, for healing).
    let ally_infos: Vec<TargetInfo> = combatants
        .iter()
        .filter(|t| t.faction.0 == Team::Enemy && t.vital.is_alive())
        .filter_map(|t| Some(build_target_info(&t, positions.get(&t.entity)?, &statuses, &db)))
        .collect();

    let eval = evaluate_targets(
        actor_pos, &actor_ctx, abilities, &enemy_infos, &ally_infos,
        &db, &difficulty, mana_cur, rage_cur, &mut rng,
    );

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
            actor, actor_pos, speed, abilities, &enemy_infos, &eval, ap,
            &actor_ctx, &db, &difficulty, mana_cur, rage_cur, &positions, &combatants,
        );

        match decision {
            MoveDecision::MoveAndAttack { path, ability, target } => {
                move_unit.write(MoveUnit { actor, path });
                use_ability.write(UseAbility { actor, ability, target });
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

// ── Context builders ──────────────────────────────────────────────────────

fn build_actor_ctx(c: &AiCombatantQItem, db: &GameDb) -> ActorCtx {
    let weapon_def = c.weapon.and_then(|w| db.weapons.get(&w.0));
    ActorCtx {
        str_mod: modifier(c.stats.strength),
        int_mod: modifier(c.stats.intelligence),
        spell_power: weapon_def.map_or(0, |wd| wd.spell_power),
        weapon_dice_expected: weapon_def.map_or(0.0, |wd| wd.dice.expected()),
    }
}

fn build_target_info(
    c: &AiCombatantQItem,
    pos: (i32, i32),
    statuses: &Query<&StatusEffects>,
    db: &GameDb,
) -> TargetInfo {
    let (armor_bonus, damage_taken_bonus) = status_bonuses(c.entity, statuses, db);
    let ctx = build_actor_ctx(c, db);
    TargetInfo {
        entity: c.entity,
        pos,
        hp: c.vital.hp,
        max_hp: c.vital.max_hp,
        armor: c.vital.armor,
        armor_bonus,
        damage_taken_bonus,
        threat: estimate_threat(&ctx, c.abilities, db),
    }
}

fn status_bonuses(entity: Entity, statuses: &Query<&StatusEffects>, db: &GameDb) -> (i32, i32) {
    let Ok(se) = statuses.get(entity) else {
        return (0, 0);
    };
    let mut armor_bonus = 0;
    let mut dmg_taken_bonus = 0;
    for active in &se.0 {
        if let Some(def) = db.statuses.get(&active.id) {
            armor_bonus += def.armor_bonus;
            dmg_taken_bonus += def.damage_taken_bonus;
        }
    }
    (armor_bonus, dmg_taken_bonus)
}

fn has_forces_targeting(
    entity: Entity,
    statuses: &Query<&StatusEffects>,
    db: &GameDb,
) -> bool {
    statuses.get(entity).map_or(false, |se| {
        se.0.iter()
            .any(|s| db.statuses.get(&s.id).map_or(false, |def| def.forces_targeting))
    })
}

// ── Evaluator ──────────────────────────────────────────────────────────────

fn evaluate_targets(
    actor_pos: (i32, i32),
    actor: &ActorCtx,
    abilities: &Abilities,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    db: &GameDb,
    difficulty: &DifficultyProfile,
    mana_cur: i32,
    rage_cur: i32,
    rng: &mut DiceRng,
) -> EvalResult {
    let mut best_in_range: Option<(AbilityId, Entity, f32)> = None;
    let mut best_any: Option<(AbilityId, Entity, f32, i32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else { continue };
        if !can_afford(def, mana_cur, rage_cur) {
            continue;
        }

        let targets: Vec<&TargetInfo> = match def.target_type {
            TargetType::SingleEnemy => enemy_infos.to_vec(),
            TargetType::SingleAlly => ally_infos.iter().collect(),
            TargetType::Myself => continue,
        };

        for target in targets {
            let base = score_action(def, target, actor, db, difficulty);
            if base <= 0.0 {
                continue;
            }
            let noise = if difficulty.noise > 0.0 {
                (rng.roll_d(1000) as f32 / 500.0 - 1.0) * difficulty.noise
            } else {
                0.0
            };
            let score = (base + noise).max(0.0);

            let dist = hex_distance(actor_pos.0, actor_pos.1, target.pos.0, target.pos.1);
            let range = def.range as i32;

            if best_any.as_ref().map_or(true, |b| score > b.2) {
                best_any = Some((ability_id.clone(), target.entity, score, range));
            }
            if dist <= range || range == 0 {
                if best_in_range.as_ref().map_or(true, |b| score > b.2) {
                    best_in_range = Some((ability_id.clone(), target.entity, score));
                }
            }
        }
    }

    EvalResult { best_in_range, best_any }
}

// ── Movement planner ──────────────────────────────────────────────────────

fn plan_movement(
    actor: Entity,
    actor_pos: (i32, i32),
    speed: &Speed,
    abilities: &Abilities,
    enemy_infos: &[&TargetInfo],
    eval: &EvalResult,
    ap: &ActionPoints,
    actor_ctx: &ActorCtx,
    db: &GameDb,
    difficulty: &DifficultyProfile,
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

    for target in enemy_infos {
        for &cell in &reach.destinations {
            if hex_distance(cell.0, cell.1, target.pos.0, target.pos.1) > target_range {
                continue;
            }
            if let Some(path) = reach.path_to(cell) {
                let is_better = best_move
                    .as_ref()
                    .map_or(true, |(bp, _)| path.len() < bp.len());
                if is_better {
                    best_move = Some((path, target.entity));
                }
            }
        }
    }

    if let Some((path, target)) = best_move {
        let dest = *path.last().unwrap();
        if ap.action {
            if let Some(target_info) = enemy_infos.iter().find(|t| t.entity == target) {
                let best_ability = pick_best_for_target(
                    target_info, dest, actor_ctx, abilities, db, difficulty, mana_cur, rage_cur,
                );
                if let Some(ability) = best_ability {
                    return MoveDecision::MoveAndAttack { path, ability, target };
                }
            }
        }
        return MoveDecision::MoveCloser { path };
    }

    // Can't reach attack range — move as close as possible.
    let mut best_approach: Option<((i32, i32), i32)> = None;
    for &cell in &reach.destinations {
        for target in enemy_infos {
            let dist = hex_distance(cell.0, cell.1, target.pos.0, target.pos.1);
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

/// Pick the best affordable ability for a specific target from a given position.
fn pick_best_for_target(
    target: &TargetInfo,
    from: (i32, i32),
    actor: &ActorCtx,
    abilities: &Abilities,
    db: &GameDb,
    difficulty: &DifficultyProfile,
    mana: i32,
    rage: i32,
) -> Option<AbilityId> {
    let mut best: Option<(AbilityId, f32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else { continue };
        if !can_afford(def, mana, rage) {
            continue;
        }
        if def.range > 0
            && hex_distance(from.0, from.1, target.pos.0, target.pos.1) > def.range as i32
        {
            continue;
        }
        let score = score_action(def, target, actor, db, difficulty);
        if score > 0.0 && best.as_ref().map_or(true, |b| score > b.1) {
            best = Some((ability_id.clone(), score));
        }
    }

    best.map(|(id, _)| id)
}
