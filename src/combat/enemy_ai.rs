#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::combat::ai_difficulty::DifficultyProfile;
use crate::combat::ai_scoring::{score_action, estimate_threat, TargetInfo};
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, TargetType};
use crate::content::races::CritFailEffect;
use crate::content::settings::GameSettings;
use crate::core::{AbilityId, DiceRng, ResourceKind};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AiCombatantQ, AiCombatantQItem,
    Combatant, Speed, StatusEffects, Team,
};
use crate::game::hex::{hex_circle, hex_line, in_bounds, Hex};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;
use hexx::EdgeDirection;
use std::collections::HashSet;

// ── Data types ─────────────────────────────────────────────────────────────

struct AiResources {
    hp: i32,
    mana: i32,
    rage: i32,
    energy: i32,
}

struct AiContext<'a> {
    db: &'a GameDb,
    difficulty: &'a DifficultyProfile,
    caster: &'a CasterContext,
    abilities: &'a Abilities,
    enemies: Vec<&'a TargetInfo>,
    allies: &'a [TargetInfo],
    resources: AiResources,
    crit_fail_effect: CritFailEffect,
    crit_fail_chance: f32,
}

struct EvalResult {
    /// Best ability+target that's within range from current position.
    /// (ability, target_pos, score)
    best_in_range: Option<(AbilityId, Hex, f32)>,
    /// Best ability+target regardless of current range (for movement planning).
    /// (ability, target_pos, score, max_range)
    best_any: Option<(AbilityId, Hex, f32, u32)>,
}

enum MoveDecision {
    MoveAndAttack {
        path: Vec<Hex>,
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveCloser {
        path: Vec<Hex>,
    },
    Stay,
}

// ── Main system ────────────────────────────────────────────────────────────

pub fn enemy_ai_system(
    db: Res<GameDb>,
    settings: Res<GameSettings>,
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
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Enemy || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    run_ai_turn(
        actor, Team::Player, &c, &db, &settings, &difficulty, &positions,
        &mut rng, &mut use_ability, &mut move_unit, &mut end_turn,
        &combatants, &statuses,
    );
}

/// Shared AI logic for both enemy_ai and pact_ai. `opponent_team` is who to attack.
fn run_ai_turn(
    actor: Entity,
    opponent_team: Team,
    c: &AiCombatantQItem,
    db: &GameDb,
    settings: &GameSettings,
    difficulty: &DifficultyProfile,
    positions: &HexPositions,
    rng: &mut DiceRng,
    use_ability: &mut MessageWriter<UseAbility>,
    move_unit: &mut MessageWriter<MoveUnit>,
    end_turn: &mut MessageWriter<EndTurn>,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses: &Query<&StatusEffects>,
) {
    if !c.ap.action && !c.ap.movement {
        end_turn.write(EndTurn { actor });
        return;
    }

    let Some(actor_pos) = positions.get(&actor) else {
        warn!("AI: actor {:?} has no position, ending turn", actor);
        end_turn.write(EndTurn { actor });
        return;
    };

    let all_opponents: Vec<TargetInfo> = combatants.iter()
        .filter(|t| t.faction.0 == opponent_team && t.vital.is_alive())
        .filter_map(|t| Some(build_target_info(&t, positions.get(&t.entity)?, statuses, db)))
        .collect();

    if all_opponents.is_empty() {
        warn!("AI: actor {:?} sees no opponents, ending turn", actor);
        end_turn.write(EndTurn { actor });
        return;
    }

    let forced: Vec<&TargetInfo> = all_opponents.iter()
        .filter(|t| has_forces_targeting(t.entity, statuses, db))
        .collect();
    let enemies: Vec<&TargetInfo> = if forced.is_empty() {
        all_opponents.iter().collect()
    } else {
        forced
    };

    let ally_infos: Vec<TargetInfo> = combatants.iter()
        .filter(|t| t.faction.0 != opponent_team && t.vital.is_alive())
        .filter_map(|t| Some(build_target_info(&t, positions.get(&t.entity)?, statuses, db)))
        .collect();

    let ctx = AiContext {
        db,
        difficulty,
        caster: &build_caster_ctx(c, db),
        abilities: c.abilities,
        enemies,
        allies: &ally_infos,
        resources: AiResources {
            hp: c.vital.hp,
            mana: c.mana.map(|m| m.current).unwrap_or(0),
            rage: c.rage.map(|r| r.current).unwrap_or(0),
            energy: c.energy.map(|e| e.current).unwrap_or(0),
        },
        crit_fail_effect: c.combat_path
            .and_then(|cp| db.paths.get(&cp.0))
            .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone()),
        crit_fail_chance: 1.0 / settings.crit_fail_die as f32,
    };

    let eval = evaluate_targets(actor_pos, &ctx, rng);

    if eval.best_in_range.is_none() && eval.best_any.is_none() {
        warn!(
            "AI: actor {:?} found no usable ability (ap: action={}, movement={}, mana={})",
            actor, c.ap.action, c.ap.movement, c.mana.map(|m| m.current).unwrap_or(0),
        );
    }

    if let Some((ref ability, target_pos, _)) = eval.best_in_range {
        if c.ap.action {
            let target = positions.entity_at(target_pos).unwrap_or(actor);
            use_ability.write(UseAbility { actor, ability: ability.clone(), target, target_pos });
            return;
        }
    }

    if c.ap.movement {
        let decision = plan_movement(
            actor, actor_pos, c.speed, c.ap, &ctx, &eval, positions, combatants,
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
    pos: Hex,
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
    statuses.get(entity).is_ok_and(|se| {
        se.0.iter()
            .any(|s| db.statuses.get(&s.id).is_some_and(|def| def.forces_targeting))
    })
}

// ── Evaluator ──────────────────────────────────────────────────────────────

fn evaluate_targets(
    actor_pos: Hex,
    ctx: &AiContext,
    rng: &mut DiceRng,
) -> EvalResult {
    let mut best_in_range: Option<(AbilityId, Hex, f32)> = None;
    let mut best_any: Option<(AbilityId, Hex, f32, u32)> = None;

    for ability_id in &ctx.abilities.0 {
        let Some(def) = ctx.db.abilities.get(ability_id) else { continue };
        if !can_afford(def, &ctx.resources) || def.target_type == TargetType::Myself {
            continue;
        }

        let max_range = def.range.max;
        let candidates = generate_candidates(actor_pos, def, ctx);

        for (target_pos, base) in candidates {
            if base <= 0.0 {
                continue;
            }
            let noise = if ctx.difficulty.noise > 0.0 {
                (rng.roll_d(1000) as f32 / 500.0 - 1.0) * ctx.difficulty.noise
            } else {
                0.0
            };
            let score = crit_fail_adjusted((base + noise).max(0.0), def, &ctx.crit_fail_effect, ctx.crit_fail_chance);

            let dist = actor_pos.unsigned_distance_to(target_pos);

            if best_any.as_ref().is_none_or(|b| score > b.2) {
                best_any = Some((ability_id.clone(), target_pos, score, max_range));
            }
            if dist <= max_range || max_range == 0 {
                let effective_score = if dist < def.range.min {
                    score * 0.65
                } else {
                    score
                };
                if best_in_range.as_ref().is_none_or(|b| effective_score > b.2) {
                    best_in_range = Some((ability_id.clone(), target_pos, effective_score));
                }
            }
        }
    }

    EvalResult { best_in_range, best_any }
}

/// Generate (target_pos, score) candidates for an ability cast from `from`.
fn generate_candidates(
    from: Hex,
    def: &AbilityDef,
    ctx: &AiContext,
) -> Vec<(Hex, f32)> {
    let max_range = def.range.max;

    let positions: Vec<Hex> = match def.aoe {
        AoEShape::None => candidates_single(def, &ctx.enemies, ctx.allies),
        AoEShape::Circle { radius } => candidates_circle(from, max_range, radius, &ctx.enemies),
        AoEShape::Line { .. } => candidates_line(from, max_range),
    };

    positions
        .into_iter()
        .map(|pos| {
            let score = match def.aoe {
                AoEShape::None => {
                    let all: Vec<&TargetInfo> = ctx.enemies.iter().copied()
                        .chain(ctx.allies.iter())
                        .collect();
                    let Some(t) = all.iter().find(|t| t.pos == pos) else { return (pos, 0.0) };
                    let mut s = score_action(def, t, ctx.caster, ctx.db, ctx.difficulty);
                    if from.unsigned_distance_to(pos) < def.range.min {
                        s *= 0.65;
                    }
                    s
                }
                _ => score_aoe(def, pos, from, ctx),
            };
            (pos, score)
        })
        .collect()
}

/// Single-target: all enemy/ally positions (range filtering is done in evaluate_targets).
fn candidates_single(
    def: &AbilityDef,
    enemy_infos: &[&TargetInfo],
    ally_infos: &[TargetInfo],
) -> Vec<Hex> {
    match def.target_type {
        TargetType::SingleEnemy => enemy_infos.iter().map(|t| t.pos).collect(),
        TargetType::SingleAlly => ally_infos.iter().map(|t| t.pos).collect(),
        TargetType::Myself => vec![],
    }
}

/// Circle AoE: ∪ hex_circle(enemy.pos, radius) ∩ {in range from actor}.
fn candidates_circle(
    from: Hex,
    max_range: u32,
    radius: u32,
    enemy_infos: &[&TargetInfo],
) -> Vec<Hex> {
    let mut centers: HashSet<Hex> = HashSet::new();
    for enemy in enemy_infos {
        for cell in hex_circle(enemy.pos, radius) {
            if max_range == 0 || from.unsigned_distance_to(cell) <= max_range {
                centers.insert(cell);
            }
        }
    }
    centers.into_iter().collect()
}

/// Line AoE: 6 directions × distances 1..=range.
fn candidates_line(from: Hex, max_range: u32) -> Vec<Hex> {
    let effective_range = if max_range == 0 { 1 } else { max_range };
    let mut results = Vec::new();
    for dir in EdgeDirection::ALL_DIRECTIONS {
        let step: Hex = dir.into();
        for d in 1..=effective_range as i32 {
            let pos = from + step * d;
            if !in_bounds(pos) {
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
    actor_pos: Hex,
    speed: &Speed,
    ap: &ActionPoints,
    ctx: &AiContext,
    eval: &EvalResult,
    positions: &HexPositions,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
) -> MoveDecision {
    let target_range = eval.best_any.as_ref().map(|b| b.3).unwrap_or(1);
    let best_target_pos = eval.best_any.as_ref().map(|b| b.1);

    // Build passability sets.
    let enemy_positions: HashSet<Hex> = combatants
        .iter()
        .filter(|c| c.entity != actor && c.faction.0 == Team::Enemy && c.vital.is_alive())
        .filter_map(|c| positions.get(&c.entity))
        .collect();
    let all_occupied: HashSet<Hex> = positions
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();

    let is_passable = |h: Hex| in_bounds(h) && !enemy_positions.contains(&h);

    let reach = reachable_with_paths(
        actor_pos,
        speed.0,
        is_passable,
        |h| !all_occupied.contains(&h),
    );

    // Find best reachable cell that puts us within ability range of any target.
    let approach_targets: Vec<Hex> = if let Some(tp) = best_target_pos {
        vec![tp]
    } else {
        ctx.enemies.iter().map(|t| t.pos).collect()
    };

    let mut best_move: Option<(Vec<Hex>, Hex)> = None;

    for &aim_pos in &approach_targets {
        for &cell in &reach.destinations {
            if cell.unsigned_distance_to(aim_pos) > target_range {
                continue;
            }
            if let Some(path) = reach.path_to(cell) {
                let is_better = best_move
                    .as_ref()
                    .is_none_or(|(bp, _)| path.len() < bp.len());
                if is_better {
                    best_move = Some((path, aim_pos));
                }
            }
        }
    }

    if let Some((path, aim_pos)) = best_move {
        let dest = *path.last().unwrap();
        if ap.action {
            let best_ability = pick_best_from_pos(dest, aim_pos, ctx);
            if let Some((ability, target_pos)) = best_ability {
                let target = positions.entity_at(target_pos).unwrap_or(actor);
                return MoveDecision::MoveAndAttack { path, ability, target, target_pos };
            }
        }
        return MoveDecision::MoveCloser { path };
    }

    // Can't reach attack range — move as close as possible to any enemy.
    let mut best_approach: Option<(Hex, u32)> = None;
    for &cell in &reach.destinations {
        for target in &ctx.enemies {
            let dist = cell.unsigned_distance_to(target.pos);
            if best_approach.is_none_or(|(_, bd)| dist < bd) {
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

/// Adjust score for critical failure probability.
/// Miss: crit chance ability does nothing → score × (1 - chance).
/// ManaOverload: ability still fires, but expected mana cost increases.
fn crit_fail_adjusted(score: f32, def: &AbilityDef, effect: &CritFailEffect, chance: f32) -> f32 {
    match effect {
        CritFailEffect::ManaOverload => {
            let mana_cost: f32 = def.costs.iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score - chance * mana_cost
        }
        CritFailEffect::CircuitBreach => {
            let mana_cost: f32 = def.costs.iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            // Miss + expected self-damage.
            score * (1.0 - chance) - chance * mana_cost * 0.5
        }
        // crit chance of miss (side effects are second-order).
        _ => score * (1.0 - chance),
    }
}

fn can_afford(def: &AbilityDef, res: &AiResources) -> bool {
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => res.hp,
            ResourceKind::Mana => res.mana,
            ResourceKind::Rage => res.rage,
            ResourceKind::Energy => res.energy,
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
    from: Hex,
    _aim_pos: Hex,
    ctx: &AiContext,
) -> Option<(AbilityId, Hex)> {
    let mut best: Option<(AbilityId, Hex, f32)> = None;

    for ability_id in &ctx.abilities.0 {
        let Some(def) = ctx.db.abilities.get(ability_id) else { continue };
        if !can_afford(def, &ctx.resources) || def.target_type == TargetType::Myself {
            continue;
        }

        for (tpos, score) in generate_candidates(from, def, ctx) {
            if score > 0.0 && best.as_ref().is_none_or(|b| score > b.2) {
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
    center_pos: Hex,
    actor_pos: Hex,
    ctx: &AiContext,
) -> f32 {
    let area: Vec<Hex> = match def.aoe {
        AoEShape::None => return 0.0,
        AoEShape::Circle { radius } => hex_circle(center_pos, radius),
        AoEShape::Line { length } => hex_line(actor_pos, center_pos, length),
    };

    let area_set: HashSet<Hex> = area.into_iter().collect();
    let mut total = 0.0f32;

    for target in &ctx.enemies {
        if area_set.contains(&target.pos) {
            total += score_action(def, target, ctx.caster, ctx.db, ctx.difficulty);
        }
    }

    if def.friendly_fire {
        for ally in ctx.allies {
            if area_set.contains(&ally.pos) {
                total -= score_action(def, ally, ctx.caster, ctx.db, ctx.difficulty).abs();
            }
        }
    }

    total
}

// ── Pact AI: AI controls hero under pact_control status ───────────────────

pub fn has_ai_control_status(entity: Entity, statuses: &Query<&StatusEffects>, db: &GameDb) -> bool {
    statuses.get(entity).is_ok_and(|se| {
        se.0.iter().any(|s| db.statuses.get(&s.id).is_some_and(|d| d.ai_controlled))
    })
}

/// AI for Player heroes under pact_control status. Attacks enemies, heals allies.
pub fn pact_ai_system(
    db: Res<GameDb>,
    settings: Res<GameSettings>,
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
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Player || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    if !has_ai_control_status(actor, &statuses, &db) {
        return;
    }
    run_ai_turn(
        actor, Team::Enemy, &c, &db, &settings, &difficulty, &positions,
        &mut rng, &mut use_ability, &mut move_unit, &mut end_turn,
        &combatants, &statuses,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef};
    use crate::core::DiceExpr;
    use crate::game::hex::hex_from_offset;

    fn dummy(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn target_at(id: u32, pos: Hex, hp: i32) -> TargetInfo {
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
            key: None,
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
            key: None,
        }
    }

    fn caster() -> CasterContext {
        CasterContext { str_mod: 2, int_mod: 3, spell_power: 1, weapon_dice: None }
    }

    fn difficulty() -> DifficultyProfile {
        DifficultyProfile::hard() // noise=0 for deterministic tests
    }

    fn test_ctx<'a>(
        caster: &'a CasterContext,
        enemies: Vec<&'a TargetInfo>,
        allies: &'a [TargetInfo],
        db: &'a GameDb,
        difficulty: &'a DifficultyProfile,
        abilities: &'a Abilities,
    ) -> AiContext<'a> {
        AiContext {
            db,
            difficulty,
            caster,
            abilities,
            enemies,
            allies,
            resources: AiResources { hp: 100, mana: 100, rage: 0, energy: 0 },
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 1.0 / 20.0,
        }
    }

    // ── candidates_circle ──────────────────────────────────────────────

    #[test]
    fn circle_candidates_include_cell_between_enemies() {
        let enemies = [target_at(0, hex_from_offset(3, 3), 10), target_at(1, hex_from_offset(5, 3), 10)];
        let refs: Vec<&TargetInfo> = enemies.iter().collect();
        let cands = candidates_circle(hex_from_offset(4, 1), 5, 1, &refs);
        assert!(cands.contains(&hex_from_offset(4, 3)), "cell between enemies should be candidate");
    }

    #[test]
    fn circle_candidates_filtered_by_range() {
        let enemies = [target_at(0, hex_from_offset(6, 5), 10)];
        let refs: Vec<&TargetInfo> = enemies.iter().collect();
        let cands = candidates_circle(hex_from_offset(0, 0), 3, 1, &refs);
        assert!(cands.is_empty());
    }

    // ── candidates_line ────────────────────────────────────────────────

    #[test]
    fn line_candidates_melee_has_6_directions() {
        let cands = candidates_line(hex_from_offset(3, 3), 1);
        assert_eq!(cands.len(), 6);
    }

    #[test]
    fn line_candidates_range2_has_12() {
        let cands = candidates_line(hex_from_offset(3, 3), 2);
        assert_eq!(cands.len(), 12);
    }

    // ── score_aoe ──────────────────────────────────────────────────────

    #[test]
    fn score_aoe_circle_hits_enemies_in_radius() {
        let def = fireball_def();
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);

        // Two enemies within radius 1 of center (3,3).
        let e1 = target_at(0, hex_from_offset(3, 3), 10);
        let e2 = target_at(1, hex_from_offset(3, 2), 10); // neighbor
        let ctx = test_ctx(&c, vec![&e1, &e2], &[], &db, &diff, &ab);

        let score = score_aoe(&def, hex_from_offset(3, 3), hex_from_offset(1, 1), &ctx);
        assert!(score > 0.0, "should hit both enemies");

        // Compare with scoring only one enemy.
        let ctx1 = test_ctx(&c, vec![&e1], &[], &db, &diff, &ab);
        let single = score_aoe(&def, hex_from_offset(3, 2), hex_from_offset(1, 1), &ctx1);
        assert!(score > single, "two enemies should score higher than one");
    }

    #[test]
    fn score_aoe_friendly_fire_reduces_score() {
        let def = fireball_def(); // friendly_fire = true
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);

        let enemy = target_at(0, hex_from_offset(3, 3), 10);
        let ally = target_at(1, hex_from_offset(3, 2), 10); // in blast radius
        let allies = [ally];

        let ctx_no_ally = test_ctx(&c, vec![&enemy], &[], &db, &diff, &ab);
        let score_no_ally = score_aoe(&def, hex_from_offset(3, 3), hex_from_offset(1, 1), &ctx_no_ally);

        let ctx_with_ally = test_ctx(&c, vec![&enemy], &allies, &db, &diff, &ab);
        let score_with_ally = score_aoe(&def, hex_from_offset(3, 3), hex_from_offset(1, 1), &ctx_with_ally);

        assert!(score_with_ally < score_no_ally, "friendly fire should reduce score");
    }

    #[test]
    fn score_aoe_line_hits_along_direction() {
        let def = line_def(); // Line { length: 2 }
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);

        let enemy = target_at(0, hex_from_offset(3, 2), 10);
        let ctx = test_ctx(&c, vec![&enemy], &[], &db, &diff, &ab);

        let score = score_aoe(&def, hex_from_offset(3, 2), hex_from_offset(3, 3), &ctx);
        assert!(score > 0.0, "enemy on line should be hit");
    }

    #[test]
    fn score_aoe_none_returns_zero() {
        let mut def = fireball_def();
        def.aoe = AoEShape::None;
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);
        let enemy = target_at(0, hex_from_offset(3, 3), 10);
        let ctx = test_ctx(&c, vec![&enemy], &[], &db, &diff, &ab);

        let score = score_aoe(&def, hex_from_offset(3, 3), hex_from_offset(1, 1), &ctx);
        assert_eq!(score, 0.0);
    }

    // ── generate_candidates ────────────────────────────────────────────

    #[test]
    fn generate_candidates_single_target_within_range() {
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);

        let mut def = fireball_def();
        def.aoe = AoEShape::None; // single target
        def.range = AbilityRange { min: 0, max: 3 };

        let near = target_at(0, hex_from_offset(3, 2), 10); // dist ~1 from (3,3)
        let far = target_at(1, hex_from_offset(6, 6), 10);  // out of range 3
        let ctx = test_ctx(&c, vec![&near, &far], &[], &db, &diff, &ab);

        let cands = generate_candidates(hex_from_offset(3, 3), &def, &ctx);
        assert_eq!(cands.len(), 2, "both enemies should be candidates (range filtering is in evaluate_targets)");
        assert!(cands.iter().any(|(pos, _)| *pos == hex_from_offset(3, 2)));
        assert!(cands.iter().any(|(pos, _)| *pos == hex_from_offset(6, 6)));
    }

    #[test]
    fn generate_candidates_circle_prefers_cluster() {
        let db = GameDb::default();
        let diff = difficulty();
        let c = caster();
        let ab = Abilities(vec![]);
        let def = fireball_def();

        // Two enemies clustered at (4,3) and (4,2), one isolated at (1,3).
        let e1 = target_at(0, hex_from_offset(4, 3), 10);
        let e2 = target_at(1, hex_from_offset(4, 2), 10);
        let e3 = target_at(2, hex_from_offset(1, 3), 10);
        let ctx = test_ctx(&c, vec![&e1, &e2, &e3], &[], &db, &diff, &ab);

        let cands = generate_candidates(hex_from_offset(3, 1), &def, &ctx);

        // Find best candidate.
        let best = cands.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap();

        // Best center should be near the cluster, not the isolated enemy.
        let dist_to_cluster = best.0.unsigned_distance_to(hex_from_offset(4, 3));
        let dist_to_isolated = best.0.unsigned_distance_to(hex_from_offset(1, 3));
        assert!(dist_to_cluster <= 1, "best center should be near cluster");
        assert!(dist_to_isolated > 1, "best center should not be near isolated enemy");
    }
}
