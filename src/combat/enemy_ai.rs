use crate::combat::ai_difficulty::DifficultyProfile;
use crate::combat::ai_scoring::{score_action, estimate_threat, TargetInfo};
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, TargetType};
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AiCombatantQ, AiCombatantQItem,
    Combatant, Speed, StatusEffects, Team,
};
use crate::game::hex::{hex_circle, hex_distance, hex_line, in_bounds};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::HashSet;

// ── Data types ─────────────────────────────────────────────────────────────

struct EvalResult {
    /// Best ability+target that's within range from current position.
    /// (ability, target_pos, score)
    best_in_range: Option<(AbilityId, (i32, i32), f32)>,
    /// Best ability+target regardless of current range (for movement planning).
    /// (ability, target_pos, score, max_range)
    best_any: Option<(AbilityId, (i32, i32), f32, i32)>,
}

enum MoveDecision {
    MoveAndAttack {
        path: Vec<(i32, i32)>,
        ability: AbilityId,
        target: Entity,
        target_pos: (i32, i32),
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
    let hp_cur = c.vital.hp;
    let mana_cur = c.mana.map(|m| m.current).unwrap_or(0);
    let rage_cur = c.rage.map(|r| r.current).unwrap_or(0);
    let energy_cur = c.energy.map(|e| e.current).unwrap_or(0);
    let caster_ctx = build_caster_ctx(&c, &db);

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
        actor_pos, &caster_ctx, abilities, &enemy_infos, &ally_infos,
        &db, &difficulty, hp_cur, mana_cur, rage_cur, energy_cur, &mut rng,
    );

    // If we have an ability+target in range, use it.
    if let Some((ref ability, target_pos, _)) = eval.best_in_range {
        if ap.action {
            let target = positions.entity_at(target_pos.0, target_pos.1).unwrap_or(actor);
            use_ability.write(UseAbility {
                actor,
                ability: ability.clone(),
                target,
                target_pos,
            });
            return;
        }
    }

    // Try to move closer and/or attack after moving.
    if ap.movement {
        let decision = plan_movement(
            actor, actor_pos, speed, abilities, &enemy_infos, &ally_infos, &eval, ap,
            &caster_ctx, &db, &difficulty, hp_cur, mana_cur, rage_cur, energy_cur, &positions, &combatants,
        );

        match decision {
            MoveDecision::MoveAndAttack { path, ability, target, target_pos } => {
                move_unit.write(MoveUnit { actor, path });
                use_ability.write(UseAbility { actor, ability, target, target_pos });
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

fn build_caster_ctx(c: &AiCombatantQItem, db: &GameDb) -> CasterContext {
    CasterContext::new(c.stats, Some(c.equipment), &db.weapons)
}

fn build_target_info(
    c: &AiCombatantQItem,
    pos: (i32, i32),
    statuses: &Query<&StatusEffects>,
    db: &GameDb,
) -> TargetInfo {
    let (armor_bonus, damage_taken_bonus) = status_bonuses(c.entity, statuses, db);
    let ctx = build_caster_ctx(c, db);
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
    actor: &CasterContext,
    abilities: &Abilities,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    db: &GameDb,
    difficulty: &DifficultyProfile,
    hp_cur: i32,
    mana_cur: i32,
    rage_cur: i32,
    energy_cur: i32,
    rng: &mut DiceRng,
) -> EvalResult {
    let mut best_in_range: Option<(AbilityId, (i32, i32), f32)> = None;
    let mut best_any: Option<(AbilityId, (i32, i32), f32, i32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else { continue };
        if !can_afford(def, hp_cur, mana_cur, rage_cur, energy_cur) {
            continue;
        }
        if def.target_type == TargetType::Myself {
            continue;
        }

        let max_range = def.range.max as i32;
        let candidates = generate_candidates(
            actor_pos, def, actor, enemy_infos, ally_infos, db, difficulty,
        );

        for (target_pos, base) in candidates {
            if base <= 0.0 {
                continue;
            }
            let noise = if difficulty.noise > 0.0 {
                (rng.roll_d(1000) as f32 / 500.0 - 1.0) * difficulty.noise
            } else {
                0.0
            };
            let score = (base + noise).max(0.0);

            let dist = hex_distance(actor_pos.0, actor_pos.1, target_pos.0, target_pos.1);

            if best_any.as_ref().map_or(true, |b| score > b.2) {
                best_any = Some((ability_id.clone(), target_pos, score, max_range));
            }
            if dist <= max_range || max_range == 0 {
                let effective_score = if dist < def.range.min as i32 {
                    score * 0.65
                } else {
                    score
                };
                if best_in_range.as_ref().map_or(true, |b| effective_score > b.2) {
                    best_in_range = Some((ability_id.clone(), target_pos, effective_score));
                }
            }
        }
    }

    EvalResult { best_in_range, best_any }
}

/// Generate (target_pos, score) candidates for an ability cast from `from`.
fn generate_candidates(
    from: (i32, i32),
    def: &AbilityDef,
    ctx: &CasterContext,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    db: &GameDb,
    difficulty: &DifficultyProfile,
) -> Vec<((i32, i32), f32)> {
    let max_range = def.range.max as i32;

    let positions: Vec<(i32, i32)> = match def.aoe {
        AoEShape::None => candidates_single(from, max_range, def, enemy_infos, ally_infos),
        AoEShape::Circle { radius } => candidates_circle(from, max_range, radius, enemy_infos),
        AoEShape::Line { .. } => candidates_line(from, max_range),
    };

    positions
        .into_iter()
        .map(|pos| {
            let score = match def.aoe {
                AoEShape::None => {
                    let all: Vec<&TargetInfo> = enemy_infos.iter().copied()
                        .chain(ally_infos.iter())
                        .collect();
                    let Some(t) = all.iter().find(|t| t.pos == pos) else { return (pos, 0.0) };
                    let mut s = score_action(def, t, ctx, db, difficulty);
                    if hex_distance(from.0, from.1, pos.0, pos.1) < def.range.min as i32 {
                        s *= 0.65;
                    }
                    s
                }
                _ => score_aoe(def, pos, from, ctx, enemy_infos, ally_infos, db, difficulty),
            };
            (pos, score)
        })
        .collect()
}

/// Single-target: enemy/ally positions within range.
fn candidates_single(
    from: (i32, i32),
    max_range: i32,
    def: &AbilityDef,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
) -> Vec<(i32, i32)> {
    let targets: Vec<(i32, i32)> = match def.target_type {
        TargetType::SingleEnemy => enemy_infos.iter().map(|t| t.pos).collect(),
        TargetType::SingleAlly => ally_infos.iter().map(|t| t.pos).collect(),
        TargetType::Myself => return vec![],
    };
    targets
        .into_iter()
        .filter(|&pos| max_range == 0 || hex_distance(from.0, from.1, pos.0, pos.1) <= max_range)
        .collect()
}

/// Circle AoE: ∪ hex_circle(enemy.pos, radius) ∩ {in range from actor}.
fn candidates_circle(
    from: (i32, i32),
    max_range: i32,
    radius: u32,
    enemy_infos: &[&TargetInfo],
) -> Vec<(i32, i32)> {
    let mut centers: HashSet<(i32, i32)> = HashSet::new();
    for enemy in enemy_infos {
        for cell in hex_circle(enemy.pos.0, enemy.pos.1, radius) {
            if max_range == 0 || hex_distance(from.0, from.1, cell.0, cell.1) <= max_range {
                centers.insert(cell);
            }
        }
    }
    centers.into_iter().collect()
}

/// Line AoE: 6 directions × distances 1..=range.
fn candidates_line(from: (i32, i32), max_range: i32) -> Vec<(i32, i32)> {
    let (ax, ay, _) = crate::game::hex::to_cube(from.0, from.1);
    let effective_range = if max_range == 0 { 1 } else { max_range };
    let mut results = Vec::new();
    for &(ux, uy, _uz) in &crate::game::hex::CUBE_DIRS {
        for d in 1..=effective_range {
            let pos = crate::game::hex::from_cube(ax + ux * d, ay + uy * d);
            if !in_bounds(pos.0, pos.1) {
                break;
            }
            results.push(pos);
        }
    }
    results
}

// ── Movement planner ──────────────────────────────────────────────────────

fn plan_movement(
    actor: Entity,
    actor_pos: (i32, i32),
    speed: &Speed,
    abilities: &Abilities,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    eval: &EvalResult,
    ap: &ActionPoints,
    caster_ctx: &CasterContext,
    db: &GameDb,
    difficulty: &DifficultyProfile,
    hp_cur: i32,
    mana_cur: i32,
    rage_cur: i32,
    energy_cur: i32,
    positions: &HexPositions,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
) -> MoveDecision {
    let target_range = eval.best_any.as_ref().map(|b| b.3).unwrap_or(1);
    let best_target_pos = eval.best_any.as_ref().map(|b| b.1);

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
    // For AoE, "target" is a position; for single-target, it's an enemy position.
    let approach_targets: Vec<(i32, i32)> = if let Some(tp) = best_target_pos {
        vec![tp]
    } else {
        enemy_infos.iter().map(|t| t.pos).collect()
    };

    let mut best_move: Option<(Vec<(i32, i32)>, (i32, i32))> = None;

    for &aim_pos in &approach_targets {
        for &cell in &reach.destinations {
            if hex_distance(cell.0, cell.1, aim_pos.0, aim_pos.1) > target_range {
                continue;
            }
            if let Some(path) = reach.path_to(cell) {
                let is_better = best_move
                    .as_ref()
                    .map_or(true, |(bp, _)| path.len() < bp.len());
                if is_better {
                    best_move = Some((path, aim_pos));
                }
            }
        }
    }

    if let Some((path, aim_pos)) = best_move {
        let dest = *path.last().unwrap();
        if ap.action {
            let best_ability = pick_best_from_pos(
                dest, aim_pos, caster_ctx, abilities, enemy_infos, ally_infos, db, difficulty, hp_cur, mana_cur, rage_cur, energy_cur,
            );
            if let Some((ability, target_pos)) = best_ability {
                let target = positions.entity_at(target_pos.0, target_pos.1).unwrap_or(actor);
                return MoveDecision::MoveAndAttack { path, ability, target, target_pos };
            }
        }
        return MoveDecision::MoveCloser { path };
    }

    // Can't reach attack range — move as close as possible to any enemy.
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

fn can_afford(def: &AbilityDef, hp: i32, mana: i32, rage: i32, energy: i32) -> bool {
    use crate::core::ResourceKind;
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => hp,
            ResourceKind::Mana => mana,
            ResourceKind::Rage => rage,
            ResourceKind::Energy => energy,
        };
        if available < cost.amount {
            return false;
        }
    }
    true
}

/// Pick the best affordable ability from position `from`.
/// Returns (ability_id, best_target_pos).
fn pick_best_from_pos(
    from: (i32, i32),
    _aim_pos: (i32, i32),
    actor: &CasterContext,
    abilities: &Abilities,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    db: &GameDb,
    difficulty: &DifficultyProfile,
    hp: i32,
    mana: i32,
    rage: i32,
    energy: i32,
) -> Option<(AbilityId, (i32, i32))> {
    let mut best: Option<(AbilityId, (i32, i32), f32)> = None;

    for ability_id in &abilities.0 {
        let Some(def) = db.abilities.get(ability_id) else { continue };
        if !can_afford(def, hp, mana, rage, energy) || def.target_type == TargetType::Myself {
            continue;
        }

        for (tpos, score) in generate_candidates(from, def, actor, enemy_infos, ally_infos, db, difficulty) {
            if score > 0.0 && best.as_ref().map_or(true, |b| score > b.2) {
                best = Some((ability_id.clone(), tpos, score));
            }
        }
    }

    best.map(|(id, pos, _)| (id, pos))
}

/// Score an AoE ability centered at `center_pos`, cast from `actor_pos`.
/// Sums score_action for enemies in the area, subtracts ally damage if friendly_fire.
fn score_aoe(
    def: &AbilityDef,
    center_pos: (i32, i32),
    actor_pos: (i32, i32),
    ctx: &CasterContext,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
    db: &GameDb,
    difficulty: &DifficultyProfile,
) -> f32 {
    let area: Vec<(i32, i32)> = match def.aoe {
        AoEShape::None => return 0.0,
        AoEShape::Circle { radius } => hex_circle(center_pos.0, center_pos.1, radius),
        AoEShape::Line { length } => hex_line(actor_pos.0, actor_pos.1, center_pos.0, center_pos.1, length),
    };

    let area_set: HashSet<(i32, i32)> = area.into_iter().collect();
    let mut total = 0.0f32;

    // Score enemies in the area (positive).
    for target in enemy_infos {
        if area_set.contains(&target.pos) {
            total += score_action(def, target, ctx, db, difficulty);
        }
    }

    // Subtract ally damage if friendly_fire.
    if def.friendly_fire {
        for ally in ally_infos {
            if area_set.contains(&ally.pos) {
                total -= score_action(def, ally, ctx, db, difficulty).abs();
            }
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef};
    use crate::core::DiceExpr;

    fn dummy(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn target_at(id: u32, pos: (i32, i32), hp: i32) -> TargetInfo {
        TargetInfo {
            entity: dummy(id),
            pos,
            hp,
            max_hp: hp,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            threat: 5.0,
        }
    }

    fn fireball_def() -> AbilityDef {
        use crate::content::abilities::ResourceCost;
        use crate::core::ResourceKind;
        AbilityDef {
            id: "fireball".into(),
            name: "Fireball".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(2, 3, 0) },
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
            aoe: AoEShape::Circle { radius: 1 },
            friendly_fire: true,
            statuses: vec![],
            magic_domains: vec!["aether".into(), "form".into()],
            magic_method: "destruction".into(),
        }
    }

    fn line_def() -> AbilityDef {
        AbilityDef {
            id: "lance".into(),
            name: "Lance".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 8, 0) },
            costs: vec![],
            aoe: AoEShape::Line { length: 2 },
            friendly_fire: false,
            statuses: vec![],
            magic_domains: vec![],
            magic_method: String::new(),
        }
    }

    fn ctx() -> CasterContext {
        CasterContext { str_mod: 2, int_mod: 3, spell_power: 1, weapon_dice: None }
    }

    fn difficulty() -> DifficultyProfile {
        DifficultyProfile::hard() // noise=0 for deterministic tests
    }

    // ── candidates_circle ──────────────────────────────────────────────

    #[test]
    fn circle_candidates_include_cell_between_enemies() {
        // Two enemies at (3,3) and (5,3): distance 2.
        // Circle radius 1 around each generates candidates.
        // Cell (4,3) is in both circles → should be a candidate.
        let enemies = [target_at(0, (3, 3), 10), target_at(1, (5, 3), 10)];
        let refs: Vec<&TargetInfo> = enemies.iter().collect();
        let cands = candidates_circle((4, 1), 5, 1, &refs);
        assert!(cands.contains(&(4, 3)), "cell between enemies should be candidate");
    }

    #[test]
    fn circle_candidates_filtered_by_range() {
        let enemies = [target_at(0, (6, 5), 10)];
        let refs: Vec<&TargetInfo> = enemies.iter().collect();
        // Actor at (0,0), range=3 — enemy at distance ~6 is out of range.
        let cands = candidates_circle((0, 0), 3, 1, &refs);
        assert!(cands.is_empty());
    }

    // ── candidates_line ────────────────────────────────────────────────

    #[test]
    fn line_candidates_melee_has_6_directions() {
        // range=1 → 6 candidates (one per hex direction)
        let cands = candidates_line((3, 3), 1);
        assert_eq!(cands.len(), 6);
    }

    #[test]
    fn line_candidates_range2_has_12() {
        // range=2 → 6 directions × 2 distances = 12 (if all in bounds)
        let cands = candidates_line((3, 3), 2);
        assert_eq!(cands.len(), 12);
    }

    // ── score_aoe ──────────────────────────────────────────────────────

    #[test]
    fn score_aoe_circle_hits_enemies_in_radius() {
        let def = fireball_def();
        let db = GameDb::default();
        let diff = difficulty();
        let ctx = ctx();

        // Two enemies within radius 1 of center (3,3).
        let e1 = target_at(0, (3, 3), 10);
        let e2 = target_at(1, (3, 2), 10); // neighbor
        let enemies = [&e1, &e2];
        let allies: Vec<TargetInfo> = vec![];

        let score = score_aoe(&def, (3, 3), (1, 1), &ctx, &enemies, &allies, &db, &diff);
        assert!(score > 0.0, "should hit both enemies");

        // Compare with scoring only one enemy.
        let single = score_aoe(&def, (3, 2), (1, 1), &ctx, &[&e1], &allies, &db, &diff);
        assert!(score > single, "two enemies should score higher than one");
    }

    #[test]
    fn score_aoe_friendly_fire_reduces_score() {
        let def = fireball_def(); // friendly_fire = true
        let db = GameDb::default();
        let diff = difficulty();
        let ctx = ctx();

        let enemy = target_at(0, (3, 3), 10);
        let ally = target_at(1, (3, 2), 10); // in blast radius
        let enemies = [&enemy];

        let score_no_ally = score_aoe(&def, (3, 3), (1, 1), &ctx, &enemies, &[], &db, &diff);
        let score_with_ally = score_aoe(&def, (3, 3), (1, 1), &ctx, &enemies, &[ally], &db, &diff);

        assert!(score_with_ally < score_no_ally, "friendly fire should reduce score");
    }

    #[test]
    fn score_aoe_line_hits_along_direction() {
        let def = line_def(); // Line { length: 2 }
        let db = GameDb::default();
        let diff = difficulty();
        let ctx = ctx();

        // Actor at (3,3), target at (3,2). Line of 2 starting at (3,2).
        // Enemy at (3,2) — should be in the line.
        let enemy = target_at(0, (3, 2), 10);
        let enemies = [&enemy];

        let score = score_aoe(&def, (3, 2), (3, 3), &ctx, &enemies, &[], &db, &diff);
        assert!(score > 0.0, "enemy on line should be hit");
    }

    #[test]
    fn score_aoe_none_returns_zero() {
        let mut def = fireball_def();
        def.aoe = AoEShape::None;
        let db = GameDb::default();
        let diff = difficulty();
        let ctx = ctx();
        let enemy = target_at(0, (3, 3), 10);

        let score = score_aoe(&def, (3, 3), (1, 1), &ctx, &[&enemy], &[], &db, &diff);
        assert_eq!(score, 0.0);
    }

    // ── generate_candidates ────────────────────────────────────────────

    #[test]
    fn generate_candidates_single_target_within_range() {
        let db = GameDb::default();
        let diff = difficulty();
        let c = ctx();

        let mut def = fireball_def();
        def.aoe = AoEShape::None; // single target
        def.range = AbilityRange { min: 0, max: 3 };

        let near = target_at(0, (3, 2), 10); // dist ~1 from (3,3)
        let far = target_at(1, (6, 6), 10);  // out of range 3
        let enemies = [&near, &far];

        let cands = generate_candidates((3, 3), &def, &c, &enemies, &[], &db, &diff);
        assert_eq!(cands.len(), 1, "only near enemy should be candidate");
        assert_eq!(cands[0].0, (3, 2));
    }

    #[test]
    fn generate_candidates_circle_prefers_cluster() {
        let db = GameDb::default();
        let diff = difficulty();
        let c = ctx();
        let def = fireball_def();

        // Two enemies clustered at (4,3) and (4,2), one isolated at (1,3).
        let e1 = target_at(0, (4, 3), 10);
        let e2 = target_at(1, (4, 2), 10);
        let e3 = target_at(2, (1, 3), 10);
        let enemies = [&e1, &e2, &e3];

        let cands = generate_candidates((3, 1), &def, &c, &enemies, &[], &db, &diff);

        // Find best candidate.
        let best = cands.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap();

        // Best center should be near the cluster, not the isolated enemy.
        let dist_to_cluster = hex_distance(best.0 .0, best.0 .1, 4, 3);
        let dist_to_isolated = hex_distance(best.0 .0, best.0 .1, 1, 3);
        assert!(dist_to_cluster <= 1, "best center should be near cluster");
        assert!(dist_to_isolated > 1, "best center should not be near isolated enemy");
    }
}
