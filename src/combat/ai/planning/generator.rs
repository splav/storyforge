//! Beam-search generation of multi-step turn plans.
//!
//! For each depth level up to `difficulty.plan_max_depth`, expand every plan in
//! the current frontier by one legal step (cast or move), score the extension
//! with a cheap proxy, and prune to `difficulty.plan_beam_width`. All plans
//! produced at any depth (including early terminations) accumulate into the
//! returned pool; Phase 3 scoring picks the winner.
//!
//! No persistent state: every tick starts fresh. Revalidation of a committed
//! plan lives in Phase 4.

use crate::combat::ai::factors::{aoe_area, aoe_hits};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::sim::SimState;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{AbilityDef, AoEShape, TargetType};
use crate::core::AbilityId;
use crate::game::hex::Hex;
use crate::game::pathfinding::ReachableMap;
use bevy::prelude::Entity;
use std::collections::{HashMap, HashSet};

// Per-step target + move-tile budgets. Composition matters more than the
// gross count: target enumeration combines a threat-ranked list (focus the
// scariest enemies) with a killability-ranked list (finish wounded ones). Move
// tile enumeration mixes escape (retreat), opportunity (attacking), and
// priority-adjacent (engage the current focus target) so the planner sees
// qualitatively different positioning options instead of five flavours of
// "retreat to safest tile".
const TARGETS_BY_THREAT: usize = 3;
const TARGETS_BY_KILLABILITY: usize = 2;
const MOVE_TILES_ESCAPE: usize = 2;
const MOVE_TILES_OPPORTUNITY: usize = 2;
const MOVE_TILES_PRIORITY_ADJACENT: usize = 1;

/// Top-level entry. Returns every plan explored during beam search: the empty
/// plan, every one-step plan, every pruned-past frontier, and the final
/// frontier. Phase 3 scores this pool uniformly.
pub fn generate_plans(
    actor: Entity,
    ctx: &UtilityContext,
    blocked_tiles: &HashSet<Hex>,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> Vec<TurnPlan> {
    let Some(actor_u) = snap.unit(actor) else {
        return Vec::new();
    };
    let max_depth = ctx.world.difficulty.plan_max_depth.max(1);
    let beam = ctx.world.difficulty.plan_beam_width.max(1);

    let seed = TurnPlan {
        steps: Vec::new(),
        final_pos: actor_u.pos,
        residual_ap: actor_u.action_points,
        residual_mp: actor_u.movement_points,
        outcomes: Vec::new(),
        partial_score: seed_partial_score(actor_u, maps),
        sim_snapshots: Vec::new(),
    };

    let mut all_plans: Vec<TurnPlan> = vec![seed.clone()];
    let mut frontier: Vec<TurnPlan> = vec![seed];

    for _ in 0..max_depth {
        let mut next: Vec<TurnPlan> = Vec::new();

        for plan in &frontier {
            // Reuse the cached sim state from the last step of this plan
            // instead of re-running `apply_step` from scratch. For the seed
            // plan (no steps), start from the original snapshot.
            let base_snapshot = plan
                .sim_snapshots
                .last()
                .cloned()
                .unwrap_or_else(|| snap.clone());
            let base_sim = SimState { snapshot: base_snapshot, actor };
            let Some(sa) = base_sim.actor_unit() else { continue };
            if sa.action_points <= 0 && sa.movement_points <= 0 {
                continue;
            }

            let steps = enumerate_next_steps(&base_sim, ctx, blocked_tiles, maps);
            for step in steps {
                // Apply this step on a cloned sim to measure outcome + state.
                let mut ext_sim = SimState {
                    snapshot: base_sim.snapshot.clone(),
                    actor,
                };
                let outcome = ext_sim.apply_step(&step, ctx.actor.caster, ctx.world.content);

                let (final_pos, residual_ap, residual_mp) = match ext_sim.actor_unit() {
                    Some(u) => (u.pos, u.action_points, u.movement_points),
                    None => (plan.final_pos, 0, 0),
                };

                let mut extended = plan.clone();
                extended.steps.push(step);
                extended.outcomes.push(outcome);
                // Cache post-step snapshot so the scorer (and the next depth
                // level here) can read it without re-simulating.
                extended.sim_snapshots.push(ext_sim.snapshot);
                extended.final_pos = final_pos;
                extended.residual_ap = residual_ap;
                extended.residual_mp = residual_mp;
                extended.partial_score = partial_score(&extended, maps);
                next.push(extended);
            }
        }

        if next.is_empty() {
            break;
        }

        next.sort_by(|a, b| b.partial_score.total_cmp(&a.partial_score));
        next.truncate(beam);

        all_plans.extend(next.iter().cloned());
        frontier = next;
    }

    dedup_by_logical_key(all_plans, actor_u.pos)
}

/// Collapse plans that differ only in movement path to the same hex. Two plans
/// with the same logical sequence (Move destinations, Cast (ability, target,
/// caster_pos) at each step) produce identical outcomes; keeping all of them
/// wastes scoring budget and eats top-K window slots with noise-only
/// differentiation — the canonical symptom: five `melee_attack Lyra at (2,4)`
/// entries taking up the top of the ranking because each uses a slightly
/// different BFS route.
///
/// When duplicates exist, keep the variant with the lowest total MP cost
/// (shorter path = more residual MP for subsequent steps). Ties keep the
/// earliest-discovered plan.
fn dedup_by_logical_key(plans: Vec<TurnPlan>, actor_start: Hex) -> Vec<TurnPlan> {
    let mut best: HashMap<Vec<StepKey>, usize> = HashMap::new();
    let mut costs: Vec<i32> = Vec::with_capacity(plans.len());
    for p in &plans {
        costs.push(total_mp_cost(p));
    }
    for (i, plan) in plans.iter().enumerate() {
        let key = logical_key(plan, actor_start);
        match best.get(&key) {
            Some(&prev) if costs[prev] <= costs[i] => {}
            _ => {
                best.insert(key, i);
            }
        }
    }
    // Preserve original order for determinism (by_key HashMap's iteration is
    // non-deterministic — sort by original index).
    let mut keepers: Vec<usize> = best.into_values().collect();
    keepers.sort_unstable();
    keepers.into_iter().map(|i| plans[i].clone()).collect()
}

/// Canonical plan key. Path variations collapse to the same key (`Move` only
/// carries its destination), but logically-distinct Cast tuples stay
/// separate. `caster_pos` is the actor's tile *at the moment of the cast* —
/// computed by walking the preceding steps.
#[derive(Hash, Eq, PartialEq, Clone)]
enum StepKey {
    Move { dest: Hex },
    Cast {
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
        caster_pos: Hex,
    },
}

fn logical_key(plan: &TurnPlan, actor_start: Hex) -> Vec<StepKey> {
    let mut pos = actor_start;
    plan.steps
        .iter()
        .map(|s| match s {
            PlanStep::Move { path } => {
                let dest = path.last().copied().unwrap_or(pos);
                pos = dest;
                StepKey::Move { dest }
            }
            PlanStep::Cast { ability, target, target_pos } => StepKey::Cast {
                ability: ability.clone(),
                target: *target,
                target_pos: *target_pos,
                caster_pos: pos,
            },
        })
        .collect()
}

fn total_mp_cost(plan: &TurnPlan) -> i32 {
    plan.steps
        .iter()
        .map(|s| match s {
            PlanStep::Move { path } => path.len() as i32,
            PlanStep::Cast { .. } => 0,
        })
        .sum()
}

// ── Step enumeration ───────────────────────────────────────────────────────

/// All legal next steps from the current sim state: castable abilities (each
/// with a top-K target set) + top-M move tiles. Bounded by MAX_* constants so
/// branching stays low even with many abilities.
fn enumerate_next_steps(
    sim: &SimState,
    ctx: &UtilityContext,
    blocked_tiles: &HashSet<Hex>,
    maps: &InfluenceMaps,
) -> Vec<PlanStep> {
    let Some(actor) = sim.actor_unit() else {
        return Vec::new();
    };
    let mut steps: Vec<PlanStep> = Vec::new();

    // Hoisted once out of the ability × target loop: which enemy (if any) is
    // taunting us? `is_valid_cast` used to re-scan all enemies per candidate,
    // making taunt-filtering quadratic over (abilities × targets).
    let taunter = sim
        .snapshot
        .enemies_of(actor.team)
        .find(|e| e.tags.contains(AiTags::FORCES_TARGETING))
        .map(|e| e.entity);

    // Cast steps from the actor's current sim position.
    for ability_id in &ctx.actor.abilities.0 {
        let Some(def) = ctx.world.content.abilities.get(ability_id) else { continue };
        if !actor.can_afford(def) {
            continue;
        }
        let targets = pick_targets(def, actor, sim);
        for (target, target_pos) in targets {
            if !is_valid_cast(def, actor, target, target_pos, sim, ctx, taunter) {
                continue;
            }
            steps.push(PlanStep::Cast {
                ability: ability_id.clone(),
                target,
                target_pos,
            });
        }
    }

    // Move steps (if MP > 0). Skipped if actor is grounded.
    if actor.movement_points > 0 {
        let reach = super::reach_from(&sim.snapshot, actor, blocked_tiles);
        let top_tiles = pick_top_move_tiles(&reach, sim, maps, actor.pos);
        for tile in top_tiles {
            if let Some(path) = reach.path_to(tile) {
                if !path.is_empty() {
                    steps.push(PlanStep::Move { path });
                }
            }
        }
    }

    steps
}

/// Hard constraints on a candidate Cast step. Rejected casts are never emitted
/// into the plan pool — they can't be scored into visibility. Mirrors the
/// legacy `filter_candidates` rules, ported here because the plan pipeline
/// replaced the candidate pipeline and never wired the filter in.
///
/// Rules:
/// - **Taunt (FORCES_TARGETING)**: any enemy with the tag forces SingleEnemy
///   casts to target a taunter; SingleAlly/Myself are unrestricted.
/// - **Overheal**: reject SingleAlly at >90% HP (no healing to be done).
/// - **Wasted CC**: reject single-target CC on an already-stunned target. AoE
///   CC keeps its candidate — dropping the whole AoE because one enemy in the
///   blast zone is stunned is wrong.
/// - **AoE friendly-fire**: reject friendly-fire AoE when allies_hit > 0 and
///   enemies_hit < allies_hit * 2.
///
/// Team safety (SingleAlly on enemy, SingleEnemy on ally) is already ensured
/// by `pick_targets` drawing from `allies_of` / `enemies_of`.
fn is_valid_cast(
    def: &AbilityDef,
    actor: &UnitSnapshot,
    target: Entity,
    target_pos: Hex,
    sim: &SimState,
    ctx: &UtilityContext,
    taunter: Option<Entity>,
) -> bool {
    // Taunt: restrict SingleEnemy to the taunter when one is active.
    if matches!(def.target_type, TargetType::SingleEnemy) {
        if let Some(t) = taunter {
            if target != t {
                return false;
            }
        }
    }

    // Overheal: SingleAlly on target above 90% HP.
    if matches!(def.target_type, TargetType::SingleAlly) {
        if let Some(t) = sim.snapshot.unit(target) {
            if t.hp_pct() > 0.9 {
                return false;
            }
        }
    }

    // Wasted single-target CC on already-stunned target.
    if applies_cc(def, ctx.world.content) && def.aoe == AoEShape::None {
        if let Some(t) = sim.snapshot.unit(target) {
            if t.tags.contains(AiTags::IS_STUNNED) {
                return false;
            }
        }
    }

    // AoE friendly-fire: allies hit without enough enemies to justify. Actor
    // counts as an ally (caster in own blast tightens the ratio).
    if def.aoe != AoEShape::None && def.friendly_fire {
        let area = aoe_area(def, target_pos, actor.pos);
        let hits = aoe_hits(&area, actor, &sim.snapshot);
        let allies_hit = hits.ally_count_with_self();
        if allies_hit > 0 && hits.enemies.len() < allies_hit * 2 {
            return false;
        }
    }

    true
}

/// Pick candidate (entity, target_pos) pairs.
///
/// - `SingleEnemy`: union of top-N by threat and top-M by killability, deduped.
///   The two signals catch qualitatively different targets — high-threat
///   scaries you want to interrupt, and nearly-dead you want to finish. Taking
///   the union avoids missing "obvious kill opportunity" when threat ranking
///   alone would push it off the list.
/// - `SingleAlly`: allies within range ranked by missing HP desc (most wounded
///   first). No separate "threat" dimension for allies.
/// - `Myself`: one pair — the actor itself.
fn pick_targets(
    def: &AbilityDef,
    actor: &UnitSnapshot,
    sim: &SimState,
) -> Vec<(Entity, Hex)> {
    let max_range = def.range.max;
    let in_range = |pos: Hex| max_range == 0 || actor.pos.unsigned_distance_to(pos) <= max_range;

    match def.target_type {
        TargetType::Myself => vec![(actor.entity, actor.pos)],
        TargetType::SingleEnemy => {
            let reachable: Vec<&UnitSnapshot> = sim
                .snapshot
                .enemies_of(actor.team)
                .filter(|u| in_range(u.pos))
                .collect();

            let mut by_threat: Vec<&UnitSnapshot> = reachable.clone();
            by_threat.sort_by(|a, b| b.threat.total_cmp(&a.threat));
            by_threat.truncate(TARGETS_BY_THREAT);

            let mut by_killability: Vec<&UnitSnapshot> = reachable;
            by_killability.sort_by(|a, b| b.killability().total_cmp(&a.killability()));
            by_killability.truncate(TARGETS_BY_KILLABILITY);

            let mut seen: HashSet<Entity> = HashSet::new();
            let mut out: Vec<(Entity, Hex)> = Vec::new();
            for u in by_threat.into_iter().chain(by_killability) {
                if seen.insert(u.entity) {
                    out.push((u.entity, u.pos));
                }
            }
            out
        }
        TargetType::SingleAlly => {
            let mut picks: Vec<(Entity, Hex, f32)> = sim
                .snapshot
                .allies_of(actor.team)
                .filter(|u| in_range(u.pos))
                .map(|u| (u.entity, u.pos, (u.max_hp - u.hp).max(0) as f32))
                .collect();
            picks.sort_by(|a, b| b.2.total_cmp(&a.2));
            picks.truncate(TARGETS_BY_THREAT + TARGETS_BY_KILLABILITY);
            picks.into_iter().map(|(e, p, _)| (e, p)).collect()
        }
    }
}

/// Diverse move-tile picker. Returns up to
/// `ESCAPE + OPPORTUNITY + PRIORITY_ADJACENT` distinct tiles, each chosen to
/// express a **different** positioning intent:
/// - top-N by escape (retreat toward support, away from danger)
/// - top-M by opportunity (approach favourable attacking lanes)
/// - one tile adjacent to the actor's current priority target (commit to
///   melee/close range on the scariest enemy)
///
/// Without this mix, the pool tends to be five flavours of "safest retreat"
/// and the planner never considers Move→Cast setups that aren't defensive.
fn pick_top_move_tiles(
    reach: &ReachableMap,
    sim: &SimState,
    maps: &InfluenceMaps,
    from: Hex,
) -> Vec<Hex> {
    let destinations: Vec<Hex> = reach
        .destinations
        .iter()
        .copied()
        .filter(|&t| t != from)
        .collect();
    if destinations.is_empty() {
        return Vec::new();
    }

    let mut by_escape: Vec<(Hex, f32)> = destinations
        .iter()
        .map(|&t| (t, maps.escape.get(t)))
        .collect();
    by_escape.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut by_opportunity: Vec<(Hex, f32)> = destinations
        .iter()
        .map(|&t| (t, maps.opportunity.get(t)))
        .collect();
    by_opportunity.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Priority-adjacent: the actor's top-priority enemy is our "engage" beacon.
    let priority_enemy = sim.actor_unit().and_then(|actor| {
        sim.snapshot
            .enemies_of(actor.team)
            .max_by(|a, b| a.threat.total_cmp(&b.threat))
    });

    let mut seen: HashSet<Hex> = HashSet::new();
    let mut out: Vec<Hex> = Vec::new();

    for (tile, _) in by_escape.iter().take(MOVE_TILES_ESCAPE) {
        if seen.insert(*tile) {
            out.push(*tile);
        }
    }
    for (tile, _) in by_opportunity.iter().take(MOVE_TILES_OPPORTUNITY) {
        if seen.insert(*tile) {
            out.push(*tile);
        }
    }
    if let Some(enemy) = priority_enemy {
        let mut adj: Vec<(Hex, f32)> = destinations
            .iter()
            .filter(|&&t| t.unsigned_distance_to(enemy.pos) == 1)
            // Tie-break by opportunity so the picked adjacent tile is the
            // best attacking position among the neighbours.
            .map(|&t| (t, maps.opportunity.get(t)))
            .collect();
        adj.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (tile, _) in adj.iter().take(MOVE_TILES_PRIORITY_ADJACENT) {
            if seen.insert(*tile) {
                out.push(*tile);
            }
        }
    }

    out
}

// ── Partial scoring (beam pruning only) ─────────────────────────────────────

/// Initial partial score for the empty plan: encourages continuing to act when
/// the actor's current tile is safe; higher danger pushes the beam to prefer
/// extensions that improve the situation.
fn seed_partial_score(actor: &UnitSnapshot, maps: &InfluenceMaps) -> f32 {
    1.0 - maps.danger.get(actor.pos)
}

/// Proxy score used for beam pruning. Deliberately cheap and lossy — the real
/// multi-factor score runs in Phase 3 against the pruned pool.
///
/// Aggregates cumulative damage/heal/kills/stuns plus a final-position safety
/// bonus. Weights prioritize *keeping good-damage plans alive* through pruning
/// over "maximally safe retreat" — we'd rather let the Phase 3 scorer reject
/// a too-aggressive plan than prune a strong-damage line at depth 1 because
/// its final tile danger shaved the score.
///
/// Calibration: 1 kill ≈ 10 HP damage ≈ 2× the pos_value spread. Heal weighted
/// like damage (symmetric support/offensive potential).
fn partial_score(plan: &TurnPlan, maps: &InfluenceMaps) -> f32 {
    let (damage, heal, kills, stuns) = plan.outcomes.iter().fold(
        (0.0f32, 0.0f32, 0usize, 0usize),
        |(d, h, k, s), o| {
            (
                d + o.damage,
                h + o.heal,
                k + o.killed.len(),
                s + o.stunned.len(),
            )
        },
    );
    let pos_value = 1.0 - maps.danger.get(plan.final_pos);

    damage * 0.1
        + heal * 0.1
        + (kills as f32) * 1.0
        + (stuns as f32) * 0.5
        + pos_value * 0.5
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use crate::content::content_view::ContentView;
    use crate::core::{AbilityId, DiceExpr};
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    fn unit(id: u32, team: Team, pos: Hex, hp: i32, max_ap: i32) -> UnitSnapshot {
        UnitSnapshot {
            entity: ent(id),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: max_ap,
            max_ap,
            movement_points: 3,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        }
    }

    fn empty_maps() -> InfluenceMaps {
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    fn empty_content() -> ContentView {
        ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    fn strike_def(id: &str, range: u32, cost_ap: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::Damage {
                dice: DiceExpr::new(1, 6, 0),
            },
            costs: Vec::new(),
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    use crate::combat::ai::test_helpers::make_test_ctx as make_ctx;

    // ── Depth-1 generation ──────────────────────────────────────────────────

    #[test]
    fn depth_1_plan_set_includes_empty_and_single_casts() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = strike_def("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_max_depth = 1;
        let caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor, target],
            round: 1,
        };
        let maps = empty_maps();

        let blocked = HashSet::<Hex>::new();
        let plans = generate_plans(actor_id, &ctx, &blocked, &snap, &maps);

        // At least one empty plan (seed) + one single-cast plan.
        assert!(plans.iter().any(|p| p.steps.is_empty()), "seed plan must exist");
        assert!(
            plans.iter().any(|p| p.steps.len() == 1
                && matches!(&p.steps[0], PlanStep::Cast { .. })),
            "at least one single-step cast plan expected"
        );
    }

    // ── Beam pruning respects width ────────────────────────────────────────

    #[test]
    fn beam_pruning_limits_per_depth_frontier() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 3);
        let mut units = vec![actor];
        // 6 targets so a naive generator would emit ≥ 6 cast candidates at
        // depth 1.
        for i in 0..6u32 {
            units.push(unit(10 + i, Team::Player, hex_from_offset(1 + i as i32, 0), 20, 1));
        }
        let actor_id = units[0].entity;

        let mut content = empty_content();
        let def = strike_def("strike", 10, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_max_depth = 2;
        difficulty.plan_beam_width = 2;
        let caster = CasterContext {
            str_mod: 0,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units,
            round: 1,
        };
        let maps = empty_maps();

        let blocked = HashSet::<Hex>::new();
        let plans = generate_plans(actor_id, &ctx, &blocked, &snap, &maps);

        // Count plans by depth. Beam=2 ⇒ depth-1 frontier size ≤ 2, depth-2 ≤ 2.
        let at_depth_1 = plans.iter().filter(|p| p.steps.len() == 1).count();
        let at_depth_2 = plans.iter().filter(|p| p.steps.len() == 2).count();
        assert!(at_depth_1 <= 2, "beam=2 should cap depth-1 frontier; got {}", at_depth_1);
        assert!(at_depth_2 <= 2, "beam=2 should cap depth-2 frontier; got {}", at_depth_2);
    }

    // ── Sim state carries into next depth: killed targets are gone ────────

    #[test]
    fn killed_target_absent_in_second_step_enumeration() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 2);
        let weak = unit(2, Team::Player, hex_from_offset(1, 0), 1, 1); // 1 HP, dies to any hit
        let other = unit(3, Team::Player, hex_from_offset(2, 0), 20, 1);
        let actor_id = actor.entity;
        let weak_id = weak.entity;

        let mut content = empty_content();
        let def = strike_def("strike", 10, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_max_depth = 2;
        difficulty.plan_beam_width = 8;
        let caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor, weak, other],
            round: 1,
        };
        let maps = empty_maps();

        let blocked = HashSet::<Hex>::new();
        let plans = generate_plans(actor_id, &ctx, &blocked, &snap, &maps);

        // Find depth-2 plans that target the weak unit first. In step 2 they
        // must not cast at weak again (it's dead post step 1).
        for p in plans.iter().filter(|p| p.steps.len() == 2) {
            let (PlanStep::Cast { target: t1, .. }, PlanStep::Cast { target: t2, .. }) =
                (&p.steps[0], &p.steps[1])
            else {
                continue;
            };
            if *t1 == weak_id {
                assert_ne!(*t2, weak_id, "step 2 must not target a unit killed in step 1");
            }
        }
    }

    // ── AP exhaustion gates extension ──────────────────────────────────────

    #[test]
    fn ap_exhaustion_stops_cast_extension() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = strike_def("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_max_depth = 3;
        difficulty.plan_beam_width = 8;
        let caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor, target],
            round: 1,
        };
        let maps = empty_maps();

        let blocked = HashSet::<Hex>::new();
        let plans = generate_plans(actor_id, &ctx, &blocked, &snap, &maps);

        // With max_ap=1, no plan should have more than one Cast step.
        for p in &plans {
            let casts = p
                .steps
                .iter()
                .filter(|s| matches!(s, PlanStep::Cast { .. }))
                .count();
            assert!(casts <= 1, "plan has {} casts but actor has 1 AP: {:?}", casts, p.steps);
        }
    }

    // ── Logical-key dedup: identical (ability, target, cast_tile) collapse ─

    #[test]
    fn dedup_collapses_same_ability_target_cast_tile() {
        let actor_start = hex_from_offset(0, 0);
        let target = ent(42);
        let cast_tile = hex_from_offset(2, 0);
        let target_pos = hex_from_offset(3, 0);
        let cost_ap = 1;

        // Three plans, all end at cast_tile and cast the same ability on the
        // same target — via three different move paths. Logically equivalent.
        let mk_plan = |path: Vec<Hex>| TurnPlan {
            steps: vec![
                PlanStep::Move { path },
                PlanStep::Cast {
                    ability: AbilityId::from("strike"),
                    target,
                    target_pos,
                },
            ],
            final_pos: cast_tile,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 1.0,
            sim_snapshots: Vec::new(),
        };
        let _ = cost_ap;

        let plans = vec![
            mk_plan(vec![hex_from_offset(1, 0), cast_tile]),
            mk_plan(vec![
                hex_from_offset(1, 0),
                hex_from_offset(1, 1),
                cast_tile,
            ]),
            mk_plan(vec![
                hex_from_offset(0, 1),
                hex_from_offset(1, 1),
                hex_from_offset(2, 1),
                cast_tile,
            ]),
        ];

        let deduped = super::dedup_by_logical_key(plans, actor_start);
        assert_eq!(
            deduped.len(),
            1,
            "three path-variants of same Cast should collapse to one",
        );
        // And the surviving one is the shortest path (2-step).
        if let PlanStep::Move { path } = &deduped[0].steps[0] {
            assert_eq!(path.len(), 2, "should keep the shortest-path variant");
        } else {
            panic!("expected Move as first step");
        }
    }

    #[test]
    fn dedup_keeps_distinct_targets() {
        let actor_start = hex_from_offset(0, 0);
        let t1 = ent(10);
        let t2 = ent(11);
        let cast_tile = hex_from_offset(2, 0);
        let mk = |target: Entity, target_pos: Hex| TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0), cast_tile] },
                PlanStep::Cast {
                    ability: AbilityId::from("strike"),
                    target,
                    target_pos,
                },
            ],
            final_pos: cast_tile,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 1.0,
            sim_snapshots: Vec::new(),
        };
        let plans = vec![
            mk(t1, hex_from_offset(3, 0)),
            mk(t2, hex_from_offset(3, 1)),
        ];
        let deduped = super::dedup_by_logical_key(plans, actor_start);
        assert_eq!(deduped.len(), 2, "distinct targets must not collapse");
    }

    // ── is_valid_cast: constraint migration from filter_candidates ─────────

    use crate::combat::ai::snapshot::AiTags as Tags;
    use crate::content::abilities::{StatusApplication, StatusOn};
    use crate::content::statuses::StatusDef;
    use crate::core::StatusId;

    fn heal_def(id: &str, range: u32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::Heal { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn stun_def(id: &str, range: u32, aoe: AoEShape) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe,
            friendly_fire: false,
            statuses: vec![StatusApplication {
                status: StatusId::from("stun"),
                duration_rounds: 1,
                on: StatusOn::Target,
            }],
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn fireball_def(id: &str, range: u32, radius: u32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius },
            friendly_fire: true,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn stun_status() -> StatusDef {
        StatusDef {
            id: StatusId::from("stun"),
            name: "stun".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
        }
    }

    fn ctx_with<'a>(
        content: &'a ContentView,
        difficulty: &'a DifficultyProfile,
        caster: &'a CasterContext,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        make_ctx(content, difficulty, caster, abilities)
    }

    // Rule 1: Taunt (FORCES_TARGETING)

    #[test]
    fn taunt_rejects_single_enemy_on_non_taunter() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let mut taunter = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        taunter.tags |= Tags::FORCES_TARGETING;
        let other = unit(3, Team::Player, hex_from_offset(0, 1), 20, 1);

        let def = strike_def("strike", 5, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), taunter.clone(), other.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        let taunter_ent = Some(taunter.entity);
        assert!(
            is_valid_cast(&def, &actor, taunter.entity, taunter.pos, &sim, &ctx, taunter_ent),
            "cast on taunter should be allowed",
        );
        assert!(
            !is_valid_cast(&def, &actor, other.entity, other.pos, &sim, &ctx, taunter_ent),
            "cast on non-taunter must be rejected under taunt",
        );
    }

    #[test]
    fn taunt_does_not_restrict_single_ally_or_myself() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 10, 1);
        let mut taunter = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        taunter.tags |= Tags::FORCES_TARGETING;
        let mut ally = unit(3, Team::Enemy, hex_from_offset(0, 1), 10, 1);
        ally.max_hp = 20; // ensure hp_pct() < 0.9 so overheal doesn't mask the test
        ally.hp = 10;

        let heal = heal_def("heal", 3);
        let mut content = empty_content();
        content.abilities.insert(heal.id.clone(), heal.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![heal.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let taunter_ent = Some(taunter.entity);
        let snap = BattleSnapshot {
            units: vec![actor.clone(), taunter, ally.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            is_valid_cast(&heal, &actor, ally.entity, ally.pos, &sim, &ctx, taunter_ent),
            "heal on wounded ally must remain valid under taunt",
        );
    }

    #[test]
    fn no_taunter_no_restriction() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let a = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let b = unit(3, Team::Player, hex_from_offset(0, 1), 20, 1);

        let def = strike_def("strike", 5, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), a.clone(), b.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(is_valid_cast(&def, &actor, a.entity, a.pos, &sim, &ctx, None));
        assert!(is_valid_cast(&def, &actor, b.entity, b.pos, &sim, &ctx, None));
    }

    // Rule 2: Overheal

    #[test]
    fn overheal_rejects_target_above_90_percent() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        // max_hp=20, hp=19 → 95%
        let mut fine = unit(2, Team::Enemy, hex_from_offset(0, 1), 19, 1);
        fine.max_hp = 20;
        // max_hp=20, hp=10 → 50%
        let mut hurt = unit(3, Team::Enemy, hex_from_offset(0, 2), 10, 1);
        hurt.max_hp = 20;

        let heal = heal_def("heal", 3);
        let mut content = empty_content();
        content.abilities.insert(heal.id.clone(), heal.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![heal.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), fine.clone(), hurt.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !is_valid_cast(&heal, &actor, fine.entity, fine.pos, &sim, &ctx, None),
            "heal on near-full ally must be rejected",
        );
        assert!(
            is_valid_cast(&heal, &actor, hurt.entity, hurt.pos, &sim, &ctx, None),
            "heal on wounded ally must be allowed",
        );
    }

    // Rule 3: Wasted CC

    #[test]
    fn wasted_single_target_cc_on_stunned_rejected() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let mut stunned = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        stunned.tags |= Tags::IS_STUNNED;
        let awake = unit(3, Team::Player, hex_from_offset(0, 1), 20, 1);

        let def = stun_def("stun_bolt", 5, AoEShape::None);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        content.statuses.insert(StatusId::from("stun"), stun_status());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), stunned.clone(), awake.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !is_valid_cast(&def, &actor, stunned.entity, stunned.pos, &sim, &ctx, None),
            "single-target CC on already-stunned target must be rejected",
        );
        assert!(
            is_valid_cast(&def, &actor, awake.entity, awake.pos, &sim, &ctx, None),
            "CC on un-stunned target must be allowed",
        );
    }

    #[test]
    fn aoe_cc_on_stunned_target_still_allowed() {
        // AoE CC keeps the candidate: dropping the whole blast because one
        // enemy in it is stunned is wrong — others in the area still benefit.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let mut stunned = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        stunned.tags |= Tags::IS_STUNNED;

        let def = stun_def("aoe_stun", 5, AoEShape::Circle { radius: 1 });
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        content.statuses.insert(StatusId::from("stun"), stun_status());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), stunned.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            is_valid_cast(&def, &actor, stunned.entity, stunned.pos, &sim, &ctx, None),
            "AoE CC must not be rejected just because the primary target is stunned",
        );
    }

    // Rule 4: AoE friendly-fire

    #[test]
    fn aoe_friendly_fire_rejected_when_hits_ally_without_enough_enemies() {
        // Fireball radius=1 centered on (1,0). Hits both (1,0) and (0,0).
        // Place an ally at (0,0) (actor itself) — allies_hit=1, enemies_hit=1
        // → need enemies_hit >= 2*allies_hit = 2 → reject.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);

        let def = fireball_def("fireball", 5, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 4, spell_power: 2, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), enemy.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !is_valid_cast(&def, &actor, enemy.entity, enemy.pos, &sim, &ctx, None),
            "friendly-fire AoE that hits self without 2x enemy value must be rejected",
        );
    }

    #[test]
    fn aoe_friendly_fire_accepted_when_enemies_outnumber_allies_two_to_one() {
        // Centre far from actor so self isn't hit. Two enemies in the blast,
        // one ally: enemies_hit=2, allies_hit=1 → 2 >= 2 → accept.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let e1 = unit(2, Team::Player, hex_from_offset(4, 0), 20, 1);
        let e2 = unit(3, Team::Player, hex_from_offset(5, 0), 20, 1);
        let ally = unit(4, Team::Enemy, hex_from_offset(4, 1), 20, 1);

        let def = fireball_def("fireball", 10, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 4, spell_power: 2, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), e1.clone(), e2.clone(), ally.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            is_valid_cast(&def, &actor, e1.entity, e1.pos, &sim, &ctx, None),
            "AoE must be accepted when enemies_hit >= 2*allies_hit",
        );
    }

    // End-to-end: confirm `generate_plans` wires the filter, not just that
    // `is_valid_cast` works in isolation.

    #[test]
    fn generate_plans_excludes_taunt_violating_casts() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let mut taunter = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
        taunter.tags |= Tags::FORCES_TARGETING;
        let adjacent_non_taunter = unit(3, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = actor.entity;
        let taunter_id = taunter.entity;

        let def = strike_def("strike", 5, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_max_depth = 1;
        let caster = CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor, taunter, adjacent_non_taunter],
            round: 1,
        };
        let maps = empty_maps();

        let blocked = HashSet::<Hex>::new();
        let plans = generate_plans(actor_id, &ctx, &blocked, &snap, &maps);

        // No plan in the pool may contain a Cast at anyone other than the taunter.
        for p in &plans {
            for step in &p.steps {
                if let PlanStep::Cast { target, .. } = step {
                    assert_eq!(
                        *target, taunter_id,
                        "plan pool leaked a non-taunter Cast: {:?}",
                        p.steps,
                    );
                }
            }
        }
    }

    // Edge case: taunted melee-only actor with out-of-reach taunter.

    #[test]
    fn taunted_actor_cannot_attack_non_taunter_even_if_taunter_unreachable() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        // Taunter is 10 hexes away — melee (range=1) cannot reach.
        let mut taunter = unit(2, Team::Player, hex_from_offset(10, 0), 20, 1);
        taunter.tags |= Tags::FORCES_TARGETING;
        let nearby = unit(3, Team::Player, hex_from_offset(1, 0), 20, 1);

        let def = strike_def("melee_attack", 1, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = ctx_with(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), taunter.clone(), nearby.clone()],
            round: 1,
        };
        let sim = SimState::from_snapshot(&snap, actor.entity);

        // The observed-incident shape: taunter out of melee reach, enemy on
        // the path. Filter must still reject the adjacent non-taunter. Actor
        // either stays in place or walks toward the taunter (handled by the
        // Move pipeline, not by this filter).
        assert!(
            !is_valid_cast(&def, &actor, nearby.entity, nearby.pos, &sim, &ctx, Some(taunter.entity)),
            "taunted melee-only actor must not attack adjacent non-taunter",
        );
    }
}
