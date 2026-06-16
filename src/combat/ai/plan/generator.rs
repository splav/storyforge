//! Beam-search generation of multi-step turn plans.
//!
//! For each depth level up to `difficulty.plan_max_depth`, expand every plan in
//! the current frontier by one legal step (cast or move), score the extension
//! with a cheap proxy, and prune to `difficulty.plan_beam_width`. All plans
//! produced at any depth (including early terminations) accumulate into the
//! returned pool; Phase 3 scoring picks the winner.
//!
//! No persistent state: every tick starts fresh. Committed-plan revalidation
//! lives in Phase 4.

use crate::combat::ai::action_state::SnapshotActionState;
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::orchestration::AiWorld;
use crate::combat::ai::outcome::builder as outcome_builder;
use crate::combat::ai::plan::sim::SimState;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::scoring::factors::{aoe_area, aoe_hits};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::bridge::entity_to_uid;
#[cfg(test)]
use crate::content::abilities::EffectCalcExt;
use crate::content::abilities::{AbilityDef, AoEShape, EffectDef, TargetType};
use crate::game::hex::Hex;
use crate::game::pathfinding::ReachableMap;
use bevy::prelude::Entity;
use combat_engine::legality::{check_legality, ProposedAction};
use combat_engine::AbilityId;
use std::collections::{HashMap, HashSet};

// Per-step target + move-tile budgets. Composition matters more than gross
// count: targets mix threat-ranked (scariest) with killability-ranked
// (finish wounded); move tiles mix escape / opportunity / priority-adjacent so
// the planner sees qualitatively different options, not five "safest retreat"s.
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
    ctx: &AiWorld,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> Vec<TurnPlan> {
    let Some(actor_u) = snap.unit(actor) else {
        return Vec::new();
    };
    let max_depth = ctx.difficulty.plan_max_depth.max(1);
    let beam = ctx.difficulty.plan_beam_width.max(1);

    let seed = TurnPlan {
        steps: Vec::new(),
        final_pos: actor_u.pos,
        residual_ap: actor_u.pools[combat_engine::PoolKind::Ap]
            .map(|(c, _)| c)
            .unwrap_or(0),
        residual_mp: actor_u.pools[combat_engine::PoolKind::Mp]
            .map(|(c, _)| c)
            .unwrap_or(0),
        outcomes: Vec::new(),
        partial_score: seed_partial_score(actor_u, maps),
        sim_snapshots: Vec::new(),
        annotation: Default::default(),
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
            let base_sim = SimState::from_snapshot(&base_snapshot, actor, ctx.status_tags);
            let Some(sa) = base_sim.actor_unit() else {
                continue;
            };
            if sa.pools[combat_engine::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0)
                <= 0
                && sa.pools[combat_engine::PoolKind::Mp]
                    .map(|(c, _)| c)
                    .unwrap_or(0)
                    <= 0
            {
                continue;
            }
            // Grab the actor's caster snapshot once — str/int/spell-power come
            // from stats + equipment, which sim doesn't mutate, so this is
            // stable for the whole plan extension loop.
            let caster_ctx = sa.cache.caster_ctx.clone();

            // Reuse the same `state` adapter the inner enumerator builds —
            // it captures `&base_sim.snapshot`, exactly the world this step
            // fires against. Re-querying check_legality here pulls the
            // disadvantage flag (disoriented status, short-range penalty)
            // for the cast about to apply.
            let pre_step_state = SnapshotActionState {
                content: ctx.content,
                snap: &base_sim.snapshot,
            };

            let steps = enumerate_next_steps(&base_sim, ctx, maps);
            for step in steps {
                // Prune Summon casts blocked by `max_active`. sim.apply_step
                // doesn't materialise summons (see sim.rs Summon branch), so
                // earlier Cast-Summon steps aren't in `base_sim.snapshot` —
                // count them from `plan.steps` and add to the live count.
                if let PlanStep::Cast { ability, .. } = &step {
                    if let Some(def) = ctx.content.abilities.get(ability) {
                        if let EffectDef::Summon { max_active, .. } = &def.effect {
                            let cap = max_active.unwrap_or(u32::MAX);
                            let actor_uid = base_sim.snapshot.uid_for_entity(actor);
                            let live = base_sim
                                .snapshot
                                .state
                                .units()
                                .iter()
                                .filter(|u| u.summoner == actor_uid && u.is_alive())
                                .count() as u32;
                            let pending = plan
                                .steps
                                .iter()
                                .filter(|s| {
                                    matches!(s,
                                        PlanStep::Cast { ability: a, .. }
                                            if ctx.content.abilities.get(a)
                                                .is_some_and(|d| matches!(d.effect,
                                                    EffectDef::Summon { .. }))
                                    )
                                })
                                .count() as u32;
                            if live + pending >= cap {
                                continue;
                            }
                        }
                    }
                }
                // Apply this step on a cloned sim to measure outcome + state.
                let mut ext_sim =
                    SimState::from_snapshot(&base_sim.snapshot, actor, ctx.status_tags);
                let disadvantage = match &step {
                    PlanStep::Cast {
                        ability,
                        target,
                        target_pos,
                    } => {
                        let proposal = ProposedAction {
                            actor,
                            ability,
                            target: *target,
                            target_pos: *target_pos,
                        };
                        check_legality(proposal, &pre_step_state)
                            .map(|legal| legal.disadvantage)
                            .unwrap_or(false)
                    }
                    PlanStep::Move { .. } => false,
                };
                let outcome = ext_sim.apply_step(&step, &caster_ctx, ctx.content, disadvantage);

                // Tick statuses at end of this branch so scoring sees
                // post-handoff state (DoT damage, expiry). Once per expansion —
                // next depth starts from the ticked snapshot, no double-tick.
                let actor_uid = entity_to_uid(actor);
                ext_sim.apply_endturn(actor_uid);

                // Mid-plan death truncation: if the actor died this step (AoO
                // lethal, self-AoE), record the plan including this step but
                // don't extend it. The depth-loop guard (`actor_unit()` → None
                // for dead actors) also prevents extension next iteration.
                let actor_is_dead = ext_sim.actor_unit().map(|a| a.hp() <= 0).unwrap_or(true);

                let (final_pos, residual_ap, residual_mp) = match ext_sim.actor_unit() {
                    Some(u) => (
                        u.pos,
                        u.pools[combat_engine::PoolKind::Ap]
                            .map(|(c, _)| c)
                            .unwrap_or(0),
                        u.pools[combat_engine::PoolKind::Mp]
                            .map(|(c, _)| c)
                            .unwrap_or(0),
                    ),
                    None => (plan.final_pos, 0, 0),
                };

                let step_damage = outcome.damage;
                // Step 4.9: outcome builder relocated to outcome::builder::from_sim_step.
                let ann_outcome = outcome_builder::from_sim_step(
                    &step,
                    &outcome,
                    step_damage,
                    &base_sim.snapshot,
                    &caster_ctx,
                    &sa.cache.crit_fail_effect,
                    ctx,
                    maps,
                    sa.pos,
                    sa.team,
                    actor,
                );
                let mut extended = plan.clone();
                extended.steps.push(step);
                extended.outcomes.push(outcome);
                // Cache post-step snapshot so the scorer (and the next depth
                // level here) can read it without re-simulating.
                // into_snapshot() moves combat_state into snapshot.state so
                // callers reading sim_snapshots.last().unit(...) see post-step values.
                extended.sim_snapshots.push(ext_sim.into_snapshot());
                // Maintain annotation.outcomes in lock-step with steps/outcomes.
                extended.annotation.outcomes.push(ann_outcome);
                extended.final_pos = final_pos;
                extended.residual_ap = residual_ap;
                extended.residual_mp = residual_mp;
                extended.partial_score = partial_score(&extended, maps);
                next.push(extended);

                // If actor died on this step, stop trying other candidates for
                // this branch — they're all equally terminal. The plan with
                // the lethal step is already recorded above.
                if actor_is_dead {
                    break;
                }
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

/// Collapse plans differing only in movement path to the same hex. Same logical
/// sequence (Move destinations + Cast (ability, target, caster_pos)) → identical
/// outcomes; keeping all wastes scoring budget and floods top-K with noise (five
/// `melee_attack Lyra at (2,4)` via slightly different BFS routes).
///
/// Keeps the lowest total-MP-cost variant (more residual MP downstream); ties
/// keep the earliest-discovered plan.
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
    Move {
        dest: Hex,
    },
    Cast {
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
        caster_pos: Hex,
    },
}

fn logical_key(plan: &TurnPlan, actor_start: Hex) -> Vec<StepKey> {
    plan.walk_with_caster(actor_start)
        .map(|(_, step, caster_pos)| match step {
            PlanStep::Move { path } => StepKey::Move {
                dest: path.last().copied().unwrap_or(caster_pos),
            },
            PlanStep::Cast {
                ability,
                target,
                target_pos,
            } => StepKey::Cast {
                ability: ability.clone(),
                target: *target,
                target_pos: *target_pos,
                caster_pos,
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
/// with a top-K target set) + top-M move tiles. Bounded by MAX_* constants.
///
/// Two filtering layers:
/// 1. **Game-rule legality** — `check_legality` (shared with player-side
///    validation): AP/resources/range/team/taunt/blocks-mana.
/// 2. **AI policy** — `ai_policy_ok` rejects legal-but-suboptimal casts
///    (overheal, wasted CC, bad AoE FF ratio) the player could still do.
fn enumerate_next_steps(sim: &SimState, ctx: &AiWorld, maps: &InfluenceMaps) -> Vec<PlanStep> {
    let Some(actor) = sim.actor_unit() else {
        return Vec::new();
    };
    let mut steps: Vec<PlanStep> = Vec::new();

    // Single ActionState adapter reused for every candidate this tick.
    let state = SnapshotActionState {
        content: ctx.content,
        snap: &sim.snapshot,
    };

    // Flee regime: offensive abilities suppressed entirely (spec §9). Must drop
    // candidates here, not just penalise in `evaluate_flee_step` — offensive
    // step-factors score independently of the intent column and would otherwise
    // let an attack win on raw damage. Self-heal/buff + moves still pass.
    let fleeing = matches!(actor.forced_mode(), Some(EvaluationMode::Flee));

    // Cast steps from the actor's current sim position. Abilities read from the
    // snapshot — same source `check_legality::actor_knows_ability` uses, so no
    // dual-list drift. `rank_targets` already runs `check_legality`, so this
    // loop only adds the AI-policy gate.
    for ability_id in &actor.cache.abilities {
        let Some(def) = ctx.content.abilities.get(ability_id) else {
            continue;
        };
        if fleeing
            && matches!(
                def.target_type,
                TargetType::SingleEnemy | TargetType::Ground | TargetType::Environment
            )
        {
            continue;
        }
        let targets = rank_targets(def, actor, sim, &state);
        for (target, target_pos) in targets {
            if !ai_policy_ok(def, actor, target, target_pos, sim, ctx) {
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
    if actor.pools[combat_engine::PoolKind::Mp]
        .map(|(c, _)| c)
        .unwrap_or(0)
        > 0
    {
        let reach = super::reach_from(&sim.snapshot, actor);
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

/// AI-policy filter for a legal Cast candidate. **Not game rules** — a human
/// player can legally do any of these; they're rejected purely because AI
/// plans through them waste AP / splash allies / redundantly stun.
///
/// Runs *after* `check_legality` accepted the candidate (target existence,
/// team, range, AP, resources, taunt already guaranteed). Heuristics inline.
fn ai_policy_ok(
    def: &AbilityDef,
    actor: UnitView<'_>,
    target: Entity,
    target_pos: Hex,
    sim: &SimState,
    ctx: &AiWorld,
) -> bool {
    // Overheal: SingleAlly on target above 90% HP.
    if matches!(def.target_type, TargetType::SingleAlly) {
        if let Some(t) = sim.unit(target) {
            if t.hp_pct() > 0.9 {
                return false;
            }
        }
    }

    // Wasted single-target CC on already-stunned target.
    if applies_cc(def, ctx.content) && def.aoe == AoEShape::None {
        if let Some(t) = sim.unit(target) {
            if t.is_stunned(ctx.status_tags) {
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

/// Rank candidate (entity, target_pos) pairs by AI heuristic, **legal candidates
/// first** (scan → keep legal → rank → top-K). Avoids the top-K-then-filter trap
/// where every top-ranked target is illegal and the ability silently yields 0
/// candidates despite a legal lower-ranked option.
///
/// - `SingleEnemy`: union of top-N by threat and top-M by killability, deduped.
///   Threat catches scaries-to-interrupt; killability catches kills-to-finish.
/// - `SingleAlly`: allies by missing HP desc (most wounded first).
/// - `Myself`: the actor itself.
fn rank_targets(
    def: &AbilityDef,
    actor: UnitView<'_>,
    sim: &SimState,
    state: &SnapshotActionState,
) -> Vec<(Entity, Hex)> {
    let ability_id = &def.id;
    let actor_entity = actor.entity();
    let is_legal = |target: Entity, target_pos: Hex| -> bool {
        let proposal = ProposedAction {
            actor: actor_entity,
            ability: ability_id,
            target,
            target_pos,
        };
        check_legality(proposal, state).is_ok()
    };

    match def.target_type {
        TargetType::Myself => {
            if is_legal(actor.entity(), actor.pos) {
                vec![(actor.entity(), actor.pos)]
            } else {
                Vec::new()
            }
        }
        TargetType::SingleEnemy => {
            // Filter to legal opponents first, then rank — top-K is now
            // K legal targets by design.
            let pool: Vec<UnitView<'_>> = sim
                .snapshot
                .enemies_of(actor.team)
                .filter(|u| is_legal(u.entity(), u.pos))
                .collect();

            let mut by_threat: Vec<UnitView<'_>> = pool.clone();
            by_threat.sort_by(|a, b| b.cache.threat.total_cmp(&a.cache.threat));
            by_threat.truncate(TARGETS_BY_THREAT);

            let mut by_killability: Vec<UnitView<'_>> = pool;
            by_killability.sort_by(|a, b| b.killability().total_cmp(&a.killability()));
            by_killability.truncate(TARGETS_BY_KILLABILITY);

            let mut seen: HashSet<Entity> = HashSet::new();
            let mut out: Vec<(Entity, Hex)> = Vec::new();
            for u in by_threat.into_iter().chain(by_killability) {
                if seen.insert(u.entity()) {
                    out.push((u.entity(), u.pos));
                }
            }
            out
        }
        TargetType::SingleAlly => {
            let mut picks: Vec<(Entity, Hex, f32)> = sim
                .snapshot
                .allies_of(actor.team)
                .filter(|u| is_legal(u.entity(), u.pos))
                .map(|u| (u.entity(), u.pos, (u.max_hp() - u.hp()).max(0) as f32))
                .collect();
            picks.sort_by(|a, b| b.2.total_cmp(&a.2));
            picks.truncate(TARGETS_BY_THREAT + TARGETS_BY_KILLABILITY);
            picks.into_iter().map(|(e, p, _)| (e, p)).collect()
        }
        // Ground: no entity target. Enumerate landing *cells* — one per
        // reachable enemy, ranked by the same threat ∪ killability union as
        // SingleEnemy. A richer cluster/cover/FF-aware picker is future work;
        // downstream scoring (`ai_policy_ok` FF ratio, offensive factors)
        // already lets a suboptimal cell lose in the beam ranking.
        TargetType::Ground => {
            let pool: Vec<UnitView<'_>> = sim
                .snapshot
                .enemies_of(actor.team)
                .filter(|u| is_legal(actor.entity(), u.pos))
                .collect();

            let mut by_threat: Vec<UnitView<'_>> = pool.clone();
            by_threat.sort_by(|a, b| b.cache.threat.total_cmp(&a.cache.threat));
            by_threat.truncate(TARGETS_BY_THREAT);

            let mut by_killability: Vec<UnitView<'_>> = pool;
            by_killability.sort_by(|a, b| b.killability().total_cmp(&a.killability()));
            by_killability.truncate(TARGETS_BY_KILLABILITY);

            // Dedupe by *cell* (not entity) — two enemies can occupy the
            // same landing rank after sorting, but hex positions are unique
            // by construction, so this is effectively a HashSet<Hex>.
            let mut seen: HashSet<Hex> = HashSet::new();
            let mut out: Vec<(Entity, Hex)> = Vec::new();
            for u in by_threat.into_iter().chain(by_killability) {
                if seen.insert(u.pos) {
                    // Ground sentinel: target entity = actor. `target_pos`
                    // is where the AoE lands.
                    out.push((actor.entity(), u.pos));
                }
            }
            out
        }
        // Environment: passive-only ability, never actively targeted.
        TargetType::Environment => Vec::new(),
    }
}

/// Diverse move-tile picker: up to `ESCAPE + OPPORTUNITY + PRIORITY_ADJACENT`
/// distinct tiles, each expressing a **different** positioning intent (retreat /
/// approach lanes / engage the priority target). Without the mix the pool
/// degenerates to five "safest retreat"s and Move→Cast setups never appear.
fn pick_top_move_tiles(
    reach: &ReachableMap,
    sim: &SimState,
    maps: &InfluenceMaps,
    from: Hex,
) -> Vec<Hex> {
    let mut destinations: Vec<Hex> = reach
        .destinations
        .iter()
        .copied()
        .filter(|&t| t != from)
        .collect();
    // Deterministic order: HashSet iteration is per-process random; without
    // sorting, the stable sort_by(score) below keeps random tie order →
    // non-deterministic chosen tile.
    destinations.sort_by(|a, b| (a.x, a.y).cmp(&(b.x, b.y)));
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
            .max_by(|a, b| a.cache.threat.total_cmp(&b.cache.threat))
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
fn seed_partial_score(actor: UnitView<'_>, maps: &InfluenceMaps) -> f32 {
    1.0 - maps.danger.get(actor.pos)
}

/// Proxy score for beam pruning — cheap and lossy; the real multi-factor score
/// runs in Phase 3. Weights keep good-damage plans alive through pruning rather
/// than favouring safe retreat (Phase 3 can still reject over-aggressive plans).
///
/// Calibration: 1 kill ≈ 10 HP damage ≈ 2× the pos_value spread; heal == damage.
fn partial_score(plan: &TurnPlan, maps: &InfluenceMaps) -> f32 {
    let (damage, heal, kills, stuns) =
        plan.outcomes
            .iter()
            .fold((0.0f32, 0.0f32, 0usize, 0usize), |(d, h, k, s), o| {
                (
                    d + o.damage,
                    h + o.heal,
                    k + o.killed.len(),
                    s + o.stunned.len(),
                )
            });
    let pos_value = 1.0 - maps.danger.get(plan.final_pos);

    damage * 0.1 + heal * 0.1 + (kills as f32) * 1.0 + (stuns as f32) * 0.5 + pos_value * 0.5
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "generator_tests.rs"]
mod tests;
