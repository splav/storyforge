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

use crate::combat::actions::{check_legality, ProposedAction};
use crate::combat::ai::action_state::SnapshotActionState;
use crate::combat::ai::factors::{aoe_area, aoe_hits};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::sim::SimState;
use crate::combat::ai::outcome::{
    ActionOutcomeEstimate, estimate_deny_value, estimate_expected_damage, estimate_kill_soon,
    estimate_rescue_value, step_path_danger,
};
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::AiWorld;
use crate::content::abilities::{AbilityDef, AoEShape, EffectDef, TargetType};
use crate::core::AbilityId;
use crate::game::components::Team;
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
        residual_ap: actor_u.action_points,
        residual_mp: actor_u.movement_points,
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
            let base_sim = SimState { snapshot: base_snapshot, actor };
            let Some(sa) = base_sim.actor_unit() else { continue };
            if sa.action_points <= 0 && sa.movement_points <= 0 {
                continue;
            }
            // Grab the actor's caster snapshot once — str/int/spell-power come
            // from stats + equipment, which sim doesn't mutate, so this is
            // stable for the whole plan extension loop.
            let caster_ctx = sa.caster_ctx.clone();

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
                // Prune Summon casts that would be blocked by `max_active`.
                // `sim.apply_step` doesn't materialise summoned units (see
                // sim.rs Summon branch), so cap pressure from *earlier*
                // Cast-Summon steps in this plan isn't visible in
                // `base_sim.snapshot` — count them from `plan.steps` and add
                // to the live count from the snapshot.
                if let PlanStep::Cast { ability, .. } = &step {
                    if let Some(def) = ctx.content.abilities.get(ability) {
                        if let EffectDef::Summon { max_active, .. } = &def.effect {
                            let cap = max_active.unwrap_or(u32::MAX);
                            let live = base_sim
                                .snapshot
                                .units
                                .iter()
                                .filter(|u| u.summoner == Some(actor) && u.is_alive())
                                .count() as u32;
                            let pending = plan
                                .steps
                                .iter()
                                .filter(|s| matches!(s,
                                    PlanStep::Cast { ability: a, .. }
                                        if ctx.content.abilities.get(a)
                                            .is_some_and(|d| matches!(d.effect,
                                                EffectDef::Summon { .. }))
                                ))
                                .count() as u32;
                            if live + pending >= cap {
                                continue;
                            }
                        }
                    }
                }
                // Apply this step on a cloned sim to measure outcome + state.
                let mut ext_sim = SimState {
                    snapshot: base_sim.snapshot.clone(),
                    actor,
                };
                let disadvantage = match &step {
                    PlanStep::Cast { ability, target, target_pos } => {
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

                let (final_pos, residual_ap, residual_mp) = match ext_sim.actor_unit() {
                    Some(u) => (u.pos, u.action_points, u.movement_points),
                    None => (plan.final_pos, 0, 0),
                };

                let step_damage = outcome.damage;
                // Step 4.2/4.3: compute all 9 ActionOutcomeEstimate fields.
                let ann_outcome = build_step_outcome_estimate(
                    &step,
                    &outcome,
                    step_damage,
                    &base_sim.snapshot,
                    &caster_ctx,
                    &sa.crit_fail_effect,
                    ctx,
                    maps,
                    sa.pos,
                    sa.team,
                );
                let mut extended = plan.clone();
                extended.steps.push(step);
                extended.outcomes.push(outcome);
                // Cache post-step snapshot so the scorer (and the next depth
                // level here) can read it without re-simulating.
                extended.sim_snapshots.push(ext_sim.snapshot);
                // Maintain annotation.outcomes in lock-step with steps/outcomes.
                extended.annotation.outcomes.push(ann_outcome);
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

/// Build the full `ActionOutcomeEstimate` for one plan step (step 4.2/4.3).
///
/// Uses the pre-step snapshot for target reads so killed targets (hp→0 in
/// `outcome.killed`) are still visible via their pre-death stats.
///
/// `caster_tile` is the actor's position before this step — needed to compute
/// the AoE blast area for multi-target deny_value and p_kill_soon aggregation.
#[allow(clippy::too_many_arguments)]
fn build_step_outcome_estimate(
    step: &PlanStep,
    outcome: &crate::combat::ai::planning::types::StepOutcome,
    step_damage: f32,
    pre_snap: &crate::combat::ai::snapshot::BattleSnapshot,
    caster: &crate::content::abilities::CasterContext,
    crit_fail_effect: &crate::content::races::CritFailEffect,
    ctx: &AiWorld,
    maps: &InfluenceMaps,
    caster_tile: Hex,
    actor_unit_team: Team,
) -> ActionOutcomeEstimate {
    match step {
        PlanStep::Cast { ability, target, target_pos } => {
            let content = ctx.content;
            let Some(def) = content.abilities.get(ability) else {
                return ActionOutcomeEstimate {
                    expected_damage: step_damage,
                    ..Default::default()
                };
            };
            let target_unit = pre_snap.unit(*target);

            // p_kill_now: 1.0 if any entity was killed by this step.
            let p_kill_now = if outcome.killed.is_empty() { 0.0 } else { 1.0 };

            // For AoE: aggregate deny_value and p_kill_soon over all enemy hits,
            // matching what compute_offensive does at scoring time.
            // For single-target: use the primary target directly.
            let (p_kill_soon, deny_value) = if def.aoe == AoEShape::None {
                let ks = target_unit.map_or(0.0, |t| estimate_kill_soon(def, t, caster, content));
                let dv = target_unit.map_or(0.0, |t| estimate_deny_value(def, t, content));
                (ks, dv)
            } else {
                let area = crate::combat::ai::factors::aoe_area(def, *target_pos, caster_tile);
                // Determine the actor's team to distinguish enemies from allies.
                let actor_team = actor_unit_team;
                let enemies_in_area: Vec<&UnitSnapshot> = pre_snap.units.iter()
                    .filter(|u| u.is_alive() && area.contains(&u.pos) && u.team != actor_team)
                    .collect();
                let ks = if enemies_in_area.iter().any(|e| estimate_kill_soon(def, e, caster, content) > 0.0) {
                    1.0
                } else {
                    0.0
                };
                let dv: f32 = enemies_in_area.iter()
                    .map(|e| estimate_deny_value(def, e, content))
                    .sum();
                (ks, dv)
            };

            // expected_damage: scorer-compatible damage estimate for single-target
            // enemy casts (= score_action + crit_fail_adjusted). For AoE, keep
            // sim-derived step_damage as a reference value — the scorer uses
            // compute_aoe_damage directly for AoE damage anyway.
            let expected_damage = if def.aoe == AoEShape::None {
                target_unit.map_or(0.0, |t| {
                    estimate_expected_damage(def, t, caster, content, crit_fail_effect, ctx.crit_fail_chance)
                })
            } else {
                step_damage
            };

            // rescue_value: heal value with urgency (only for SingleAlly).
            let danger_at_target = maps.danger.get(*target_pos);
            let rescue_value = target_unit.map_or(0.0, |t| {
                estimate_rescue_value(
                    def, t, caster, content, danger_at_target,
                    crit_fail_effect, ctx.crit_fail_chance,
                )
            });

            // resource_swing: -(AP cost) - (mana/rage/energy costs).
            let resource_swing = -(def.cost_ap as f32)
                - def.costs.iter().map(|c| c.amount as f32).sum::<f32>();

            ActionOutcomeEstimate {
                expected_damage,
                p_kill_now,
                p_kill_soon,
                deny_value,
                rescue_value,
                board_pressure: 0.0,  // filled in step 5
                exposure_delta: 0.0,  // Cast: no movement, no path danger
                geometry_gain: 0.0,   // filled in step 17
                resource_swing,
            }
        }
        PlanStep::Move { path } => {
            ActionOutcomeEstimate {
                expected_damage: 0.0,
                p_kill_now: 0.0,
                p_kill_soon: 0.0,
                deny_value: 0.0,
                rescue_value: 0.0,
                board_pressure: 0.0,   // filled in step 5
                exposure_delta: step_path_danger(step, maps),
                geometry_gain: 0.0,    // filled in step 17
                resource_swing: -(path.len() as f32),
            }
        }
    }
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
    plan.walk_with_caster(actor_start)
        .map(|(_, step, caster_pos)| match step {
            PlanStep::Move { path } => StepKey::Move {
                dest: path.last().copied().unwrap_or(caster_pos),
            },
            PlanStep::Cast { ability, target, target_pos } => StepKey::Cast {
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
/// with a top-K target set) + top-M move tiles. Bounded by MAX_* constants so
/// branching stays low even with many abilities.
///
/// Two layers of filtering:
/// 1. **Game-rule legality** — `check_legality` (shared with player-side
///    validation) handles AP/resources/range/team/taunt/blocks-mana-status
///    uniformly. Anything it rejects is simply not a legal action; we never
///    waste a beam slot on it.
/// 2. **AI policy** — `ai_policy_ok` is the heuristic layer that rejects
///    legal-but-suboptimal casts (overheal, wasted CC, bad AoE FF ratio).
///    Player is free to do any of these; AI doesn't plan through them.
fn enumerate_next_steps(
    sim: &SimState,
    ctx: &AiWorld,
    maps: &InfluenceMaps,
) -> Vec<PlanStep> {
    let Some(actor) = sim.actor_unit() else {
        return Vec::new();
    };
    let mut steps: Vec<PlanStep> = Vec::new();

    // Single ActionState adapter reused for every candidate this tick.
    let state = SnapshotActionState {
        content: ctx.content,
        snap: &sim.snapshot,
    };

    // Cast steps from the actor's current sim position. Read abilities
    // from the snapshot — same source `check_legality::actor_knows_ability`
    // will consult, so no dual-list drift. `rank_targets` already filters
    // candidates through `check_legality`, so this loop only needs the
    // AI-policy gate on top.
    for ability_id in &actor.abilities {
        let Some(def) = ctx.content.abilities.get(ability_id) else { continue };
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
    if actor.movement_points > 0 {
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
/// Runs *after* `check_legality` accepted the candidate (so actor/target
/// existence, team, range, AP, resources, taunt are already guaranteed).
///
/// Heuristics:
/// - **Overheal**: SingleAlly on target above 90% HP — almost no healing
///   to be done.
/// - **Wasted CC**: single-target CC on an already-stunned target. AoE CC
///   keeps its candidate — dropping the whole AoE because one enemy in
///   the blast zone is stunned is wrong.
/// - **AoE friendly-fire ratio**: reject friendly-fire AoE when
///   `allies_hit > 0 && enemies_hit < allies_hit * 2` (splash damages
///   more friends than enemies justify).
fn ai_policy_ok(
    def: &AbilityDef,
    actor: &UnitSnapshot,
    target: Entity,
    target_pos: Hex,
    sim: &SimState,
    ctx: &AiWorld,
) -> bool {
    // Overheal: SingleAlly on target above 90% HP.
    if matches!(def.target_type, TargetType::SingleAlly) {
        if let Some(t) = sim.snapshot.unit(target) {
            if t.hp_pct() > 0.9 {
                return false;
            }
        }
    }

    // Wasted single-target CC on already-stunned target.
    if applies_cc(def, ctx.content) && def.aoe == AoEShape::None {
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

/// Rank candidate (entity, target_pos) pairs by AI heuristic, **filtered to
/// legal candidates first**. Closes the old top-K-then-filter trap where
/// every top-ranked target could be illegal (out-of-range / taunt-blocked /
/// dead) and the ability silently produced 0 candidates even with a legal
/// lower-ranked option in the pool.
///
/// Order is: scan candidates → keep legal → rank → top-K. Legality is
/// queried via the same `check_legality` / `SnapshotActionState` pair
/// `enumerate_next_steps` would have called downstream, so the upstream
/// check is now redundant and the caller drops it.
///
/// - `SingleEnemy`: union of top-N by threat and top-M by killability,
///   deduped. Two signals catch qualitatively different targets — high-
///   threat scaries you want to interrupt, and nearly-dead you want to
///   finish. Union avoids missing "obvious kill opportunity" when threat
///   alone would push it off the list.
/// - `SingleAlly`: allies ranked by missing HP desc (most wounded first).
///   No separate "threat" dimension for allies.
/// - `Myself`: one pair — the actor itself.
fn rank_targets(
    def: &AbilityDef,
    actor: &UnitSnapshot,
    sim: &SimState,
    state: &SnapshotActionState,
) -> Vec<(Entity, Hex)> {
    let ability_id = &def.id;
    let actor_entity = actor.entity;
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
            if is_legal(actor.entity, actor.pos) {
                vec![(actor.entity, actor.pos)]
            } else {
                Vec::new()
            }
        }
        TargetType::SingleEnemy => {
            // Filter to legal opponents first, then rank — top-K is now
            // K legal targets by design.
            let pool: Vec<&UnitSnapshot> = sim
                .snapshot
                .enemies_of(actor.team)
                .filter(|u| is_legal(u.entity, u.pos))
                .collect();

            let mut by_threat: Vec<&UnitSnapshot> = pool.clone();
            by_threat.sort_by(|a, b| b.threat.total_cmp(&a.threat));
            by_threat.truncate(TARGETS_BY_THREAT);

            let mut by_killability: Vec<&UnitSnapshot> = pool;
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
                .filter(|u| is_legal(u.entity, u.pos))
                .map(|u| (u.entity, u.pos, (u.max_hp - u.hp).max(0) as f32))
                .collect();
            picks.sort_by(|a, b| b.2.total_cmp(&a.2));
            picks.truncate(TARGETS_BY_THREAT + TARGETS_BY_KILLABILITY);
            picks.into_iter().map(|(e, p, _)| (e, p)).collect()
        }
        // Ground: no entity target. Enumerate candidate landing *cells*.
        // Simplest working heuristic: enemy-centered — one candidate cell
        // per reachable enemy, ranked by the same threat ∪ killability
        // union as SingleEnemy. Matches how pre-conversion thunderstrike
        // was ranked (targetted an enemy, AoE fell on their tile); after
        // thunderstrike → Ground the AI keeps the same tactical shape
        // without needing a globally optimal cluster-picker.
        //
        // A richer scorer (centroid of enemy clusters, cover avoidance,
        // friendly-fire minimisation) is a future refinement. The scoring
        // pipeline downstream already penalises bad AoE footprints via
        // `ai_policy_ok` (friendly-fire ratio) and `offensive` factors,
        // so a suboptimal landing cell still loses to a better one in the
        // beam-search ranking — we don't need to bake that into enumeration.
        TargetType::Ground => {
            let pool: Vec<&UnitSnapshot> = sim
                .snapshot
                .enemies_of(actor.team)
                .filter(|u| is_legal(actor.entity, u.pos))
                .collect();

            let mut by_threat: Vec<&UnitSnapshot> = pool.clone();
            by_threat.sort_by(|a, b| b.threat.total_cmp(&a.threat));
            by_threat.truncate(TARGETS_BY_THREAT);

            let mut by_killability: Vec<&UnitSnapshot> = pool;
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
                    out.push((actor.entity, u.pos));
                }
            }
            out
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
    use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, ent, UnitBuilder};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr};
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::hex_from_offset;

    /// Generator-suite defaults: caller sets `hp` + `max_ap` (beam search
    /// branching tests rely on these to tune pool shape). Ability list is a
    /// test-wide superset of every id referenced across tests in this
    /// module, so each test actor "knows" whatever a specific test wires
    /// through `ctx.actor.abilities` without per-test ability setup.
    /// Tests that specifically exercise unknown-ability rejection use
    /// `UnitBuilder::ability_names(&[])` directly.
    fn unit(id: u32, team: Team, pos: Hex, hp: i32, max_ap: i32) -> UnitSnapshot {
        UnitBuilder::new(id, team, pos)
            .hp(hp)
            .ap(max_ap)
            .ability_names(&[
                "strike", "melee_attack", "heal", "stun_bolt", "aoe_stun",
                "fireball", "mana_bolt", "melee",
            ])
            .build()
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

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let _caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, target], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        // At least one empty plan (seed) + one single-cast plan.
        assert!(plans.iter().any(|p| p.steps.is_empty()), "seed plan must exist");
        assert!(
            plans.iter().any(|p| p.steps.len() == 1
                && matches!(&p.steps[0], PlanStep::Cast { .. })),
            "at least one single-step cast plan expected"
        );
        // Invariant: annotation.outcomes.len() == steps.len() for every plan.
        for plan in &plans {
            assert_eq!(
                plan.annotation.outcomes.len(),
                plan.steps.len(),
                "annotation.outcomes length must match steps length"
            );
        }
    }

    // ── Annotation outcomes match sim outcomes ─────────────────────────────

    #[test]
    fn annotation_expected_damage_matches_estimate_expected_damage() {
        // Step 4.3: `annotation.expected_damage` stores the scorer-compatible
        // expected damage (= score_action() output via estimate_expected_damage),
        // NOT the sim's actual rolled damage in `outcome.damage`.
        // This test verifies that generator fills `annotation.expected_damage`
        // with the same value `estimate_expected_damage` would return.
        use crate::combat::ai::outcome::estimate_expected_damage;
        use crate::content::abilities::CasterContext;
        use crate::content::races::CritFailEffect;

        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = strike_def("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        let crit_fail_effect = CritFailEffect::default();
        let caster_ctx = CasterContext::default();
        // Expected damage as the scorer would compute it for this def + target.
        let ref_damage = estimate_expected_damage(
            &def, &target, &caster_ctx, &content, &crit_fail_effect, 0.0,
        );

        // Every Cast plan: annotation.expected_damage must equal the scorer's
        // expected damage formula (strict f32 equality — same formula, no rounding).
        for plan in plans.iter().filter(|p| !p.steps.is_empty()) {
            for (i, ann) in plan.annotation.outcomes.iter().enumerate() {
                if !matches!(plan.steps.get(i), Some(PlanStep::Cast { .. })) {
                    continue;
                }
                assert_eq!(
                    ann.expected_damage,
                    ref_damage,
                    "plan step {i}: annotation.expected_damage ({}) != estimate_expected_damage ({})",
                    ann.expected_damage,
                    ref_damage
                );
            }
        }
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

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 2;
        difficulty.plan_beam_width = 2;
        let _caster = CasterContext {
            str_mod: 0,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(units, 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

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

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 2;
        difficulty.plan_beam_width = 8;
        let _caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, weak, other], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

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

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 3;
        difficulty.plan_beam_width = 8;
        let _caster = CasterContext {
            str_mod: 4,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
        };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, target], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

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
            annotation: Default::default(),
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
            annotation: Default::default(),
        };
        let plans = vec![
            mk(t1, hex_from_offset(3, 0)),
            mk(t2, hex_from_offset(3, 1)),
        ];
        let deduped = super::dedup_by_logical_key(plans, actor_start);
        assert_eq!(deduped.len(), 2, "distinct targets must not collapse");
    }

    // ── ai_policy_ok: AI heuristic layer (overheal, wasted CC, AoE FF ratio) ───
    //
    // Game-rule cases (taunt, team-safety, blocks_mana_abilities, range)
    // are covered at the `check_legality` layer (actions/mod.rs + arch
    // D.a) and end-to-end via `generate_plans_*` tests below.

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
            buff_class: None,
        }
    }


    // Rule 1: Overheal

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
        let difficulty = DifficultyProfile::hard();
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![heal.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), fine.clone(), hurt.clone()], 1);
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !ai_policy_ok(&heal, &actor, fine.entity, fine.pos, &sim, &ctx),
            "heal on near-full ally must be rejected",
        );
        assert!(
            ai_policy_ok(&heal, &actor, hurt.entity, hurt.pos, &sim, &ctx),
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

        let difficulty = DifficultyProfile::hard();
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), stunned.clone(), awake.clone()], 1);
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !ai_policy_ok(&def, &actor, stunned.entity, stunned.pos, &sim, &ctx),
            "single-target CC on already-stunned target must be rejected",
        );
        assert!(
            ai_policy_ok(&def, &actor, awake.entity, awake.pos, &sim, &ctx),
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

        let difficulty = DifficultyProfile::hard();
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), stunned.clone()], 1);
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            ai_policy_ok(&def, &actor, stunned.entity, stunned.pos, &sim, &ctx),
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
        let difficulty = DifficultyProfile::hard();
        let _caster = CasterContext { str_mod: 0, int_mod: 4, spell_power: 2, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), enemy.clone()], 1);
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            !ai_policy_ok(&def, &actor, enemy.entity, enemy.pos, &sim, &ctx),
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
        let difficulty = DifficultyProfile::hard();
        let _caster = CasterContext { str_mod: 0, int_mod: 4, spell_power: 2, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor.clone(), e1.clone(), e2.clone(), ally.clone()], 1);
        let sim = SimState::from_snapshot(&snap, actor.entity);

        assert!(
            ai_policy_ok(&def, &actor, e1.entity, e1.pos, &sim, &ctx),
            "AoE must be accepted when enemies_hit >= 2*allies_hit",
        );
    }

    // End-to-end: confirm `generate_plans` wires the legality + policy
    // filters, not just that they work in isolation.

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

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let _caster = CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, taunter, adjacent_non_taunter], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

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

    /// Regression: AI must respect `blocks_mana_abilities` at planning time.
    /// Pre-arch-D the planner only checked `can_afford` (AP + resource amount),
    /// missing the status flag — so a unit under `broken_faith` would plan
    /// mana-cost casts, lose the round to validation's reject, and `EndTurn`.
    /// Now `check_legality` gates every Cast candidate and filters them out.
    #[test]
    fn generate_plans_excludes_mana_casts_under_blocks_mana_status() {
        use crate::combat::ai::snapshot::ActiveStatusView;
        use crate::core::ResourceKind;

        // Actor has broken_faith + enough mana + both a mana spell and a
        // no-cost melee fallback.
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 2);
        actor.mana = Some((10, 10));
        actor.statuses.push(ActiveStatusView {
            id: StatusId::from("broken_faith"),
            rounds_remaining: 3,
            dot_per_tick: 0,
        });
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = actor.entity;

        let mut mana_bolt = strike_def("mana_bolt", 5, 1);
        mana_bolt.costs = vec![crate::content::abilities::ResourceCost {
            resource: ResourceKind::Mana,
            amount: 5,
        }];
        let melee = strike_def("melee", 1, 1);

        let mut content = empty_content();
        content.abilities.insert(mana_bolt.id.clone(), mana_bolt.clone());
        content.abilities.insert(melee.id.clone(), melee.clone());
        content.statuses.insert(
            StatusId::from("broken_faith"),
            StatusDef {
                id: StatusId::from("broken_faith"),
                name: "broken_faith".into(),
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: false,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: true,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: false,
                buff_class: None,
            },
        );

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![mana_bolt.id.clone(), melee.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, enemy], 1);
        let maps = empty_maps();
        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        // No plan may use the mana spell.
        let mana_id = mana_bolt.id.clone();
        for p in &plans {
            for step in &p.steps {
                if let PlanStep::Cast { ability, .. } = step {
                    assert_ne!(
                        *ability, mana_id,
                        "broken_faith must filter mana casts out of the plan pool",
                    );
                }
            }
        }

        // Sanity: plans with the melee fallback are still there — AI
        // doesn't starve.
        let melee_id = melee.id.clone();
        let has_melee = plans.iter().any(|p| {
            p.steps.iter().any(|s| {
                matches!(s, PlanStep::Cast { ability, .. } if *ability == melee_id)
            })
        });
        assert!(has_melee, "non-mana fallback cast must still be available");
    }

    /// Ground-targeted abilities: generator must enumerate candidate
    /// landing cells (one per in-range enemy), emitting
    /// `(actor_entity, enemy.pos)` pairs — target entity is the actor
    /// sentinel, target_pos is where the AoE lands. Regression guard for
    /// the phase-1 empty-candidates stub: without this, AI can never cast
    /// fireball / thunderstrike post-Ground-conversion.
    #[test]
    fn ground_generator_emits_enemy_centered_cells() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let actor_id = actor.entity;
        let enemy_a = unit(2, Team::Player, hex_from_offset(3, 0), 20, 1);
        let enemy_b = unit(3, Team::Player, hex_from_offset(0, 3), 20, 1);
        let enemy_a_pos = enemy_a.pos;
        let enemy_b_pos = enemy_b.pos;

        let fireball = AbilityDef {
            id: AbilityId::from("fireball"),
            name: "fireball".into(),
            target_type: TargetType::Ground,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(2, 3, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius: 1 },
            friendly_fire: true,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };

        let mut content = empty_content();
        content.abilities.insert(fireball.id.clone(), fireball.clone());
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let ctx = make_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor, enemy_a, enemy_b], 1);
        let maps = empty_maps();

        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        let fireball_id = fireball.id.clone();
        let landed_cells: HashSet<Hex> = plans
            .iter()
            .flat_map(|p| p.steps.iter())
            .filter_map(|s| match s {
                PlanStep::Cast { ability, target, target_pos }
                    if *ability == fireball_id =>
                {
                    // Sentinel check: Ground uses actor as target entity.
                    assert_eq!(*target, actor_id, "Ground Cast target must be actor sentinel");
                    Some(*target_pos)
                }
                _ => None,
            })
            .collect();

        assert!(
            landed_cells.contains(&enemy_a_pos),
            "enemy A's cell must be a landing candidate (landed: {landed_cells:?})",
        );
        assert!(
            landed_cells.contains(&enemy_b_pos),
            "enemy B's cell must be a landing candidate (landed: {landed_cells:?})",
        );
    }

    /// Regression for arch-debt-A: when the top-K-by-rank enemies are all
    /// illegal (out-of-range / taunt-blocked), the planner must still
    /// surface a legal lower-ranked target. Pre-fix, `rank_targets` picked
    /// top-K first then `check_legality` dropped them all → 0 candidates
    /// even though a legal target existed in the pool.
    ///
    /// Setup: 3 high-threat enemies (top-K candidates) all out of strike
    /// range, plus 1 low-threat enemy in range. Expectation: planner
    /// generates a Cast at the in-range enemy.
    #[test]
    fn rank_targets_picks_legal_when_top_k_by_rank_all_illegal() {
        // Strike range = 1, melee. High-threat enemies parked out of reach.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let actor_id = actor.entity;
        let mut far1 = unit(2, Team::Player, hex_from_offset(8, 0), 20, 1);
        far1.threat = 100.0;
        let mut far2 = unit(3, Team::Player, hex_from_offset(7, 1), 20, 1);
        far2.threat = 90.0;
        let mut far3 = unit(4, Team::Player, hex_from_offset(8, 2), 20, 1);
        far3.threat = 80.0;
        // The only legal target — adjacent, low threat.
        let mut close = unit(5, Team::Player, hex_from_offset(1, 0), 20, 1);
        close.threat = 1.0;
        let close_id = close.entity;

        let def = strike_def("strike", 1, 1);
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let _caster = CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(
            vec![actor, far1, far2, far3, close],
            1,
        );
        let maps = empty_maps();
        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        // The legal close enemy must surface as a Cast in some plan.
        let strike_id = def.id.clone();
        let has_close_cast = plans.iter().any(|p| {
            p.steps.iter().any(|s| {
                matches!(s, PlanStep::Cast { ability, target, .. }
                         if *ability == strike_id && *target == close_id)
            })
        });
        assert!(
            has_close_cast,
            "rank_targets must dig past illegal top-K to find the legal close target",
        );
    }

    /// Disadvantage (from `causes_disadvantage` status) must discount the
    /// damage estimate on every Cast step in generated plans. Baseline
    /// (no status) vs dis-status run of the same setup: dis damage should
    /// be strictly less. Closes arch-audit divergence A2 — AI was
    /// over-estimating disoriented unit's damage.
    #[test]
    fn disadvantage_status_discounts_plan_damage_estimate() {
        use crate::combat::ai::snapshot::ActiveStatusView;
        use crate::content::statuses::StatusDef;
        use crate::core::StatusId;

        let base_actor = || unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
        let actor_id = base_actor().entity;

        let def = AbilityDef {
            id: AbilityId::from("strike"),
            name: "strike".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(2, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };

        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());
        content.statuses.insert(
            StatusId::from("disoriented"),
            StatusDef {
                id: StatusId::from("disoriented"),
                name: "disoriented".into(),
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: false,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: true,
                buff_class: None,
            },
        );

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let ctx = make_ctx(&content, &difficulty);
        let maps = empty_maps();

        // Baseline: no status.
        let snap_base = BattleSnapshot::new(vec![base_actor(), target.clone()], 1);
        let plans_base = generate_plans(actor_id, &ctx, &snap_base, &maps);
        let dmg_base: f32 = cast_damage_sum(&plans_base);

        // Under disadvantage status.
        let mut dis_actor = base_actor();
        dis_actor.statuses.push(ActiveStatusView {
            id: StatusId::from("disoriented"),
            rounds_remaining: 3,
            dot_per_tick: 0,
        });
        let snap_dis = BattleSnapshot::new(vec![dis_actor, target], 1);
        let plans_dis = generate_plans(actor_id, &ctx, &snap_dis, &maps);
        let dmg_dis: f32 = cast_damage_sum(&plans_dis);

        assert!(
            dmg_base > 0.0 && dmg_dis > 0.0,
            "both runs must generate at least one Cast plan (base={dmg_base}, dis={dmg_dis})",
        );
        assert!(
            dmg_dis < dmg_base,
            "disadvantage must discount damage: base={dmg_base}, dis={dmg_dis}",
        );
    }

    /// Summon cap must prune Cast candidates when live summons already fill
    /// the slot. Regression guard for the bug where `SummonedBy` survived
    /// death → AI planned a cast that would be blocked by spawn at runtime,
    /// wasting AP.
    #[test]
    fn generate_plans_excludes_summon_when_cap_reached() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).ap(2).ability_names(&["summon_spirit"]).build();
        let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
        let s1 = UnitBuilder::new(3, Team::Enemy, hex_from_offset(1, 0))
            .hp(10).summoner(actor.entity).ability_names(&[]).build();
        let s2 = UnitBuilder::new(4, Team::Enemy, hex_from_offset(0, 1))
            .hp(10).summoner(actor.entity).ability_names(&[]).build();
        let actor_id = actor.entity;

        let summon_def = AbilityDef {
            id: AbilityId::from("summon_spirit"),
            name: "summon_spirit".into(),
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template: "spirit".into(),
                max_active: Some(2),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };

        let mut content = empty_content();
        content.abilities.insert(summon_def.id.clone(), summon_def.clone());
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 2;
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, enemy, s1, s2], 1);
        let maps = empty_maps();
        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        for p in &plans {
            for step in &p.steps {
                if let PlanStep::Cast { ability, .. } = step {
                    assert_ne!(
                        *ability, summon_def.id,
                        "cap-reached summon must be pruned from plan pool",
                    );
                }
            }
        }
    }

    /// Dead summons must NOT occupy a cap slot: with cap=2, one live + one
    /// dead summon leaves room for one more. Mirrors the spawn-side fix.
    #[test]
    fn generate_plans_allows_summon_when_only_dead_summons_fill_slots() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).ap(2).ability_names(&["summon_spirit"]).build();
        let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
        let alive = UnitBuilder::new(3, Team::Enemy, hex_from_offset(1, 0))
            .hp(10).summoner(actor.entity).ability_names(&[]).build();
        // hp=0 ⇒ !is_alive(), should be excluded from the cap count.
        let dead = UnitBuilder::new(4, Team::Enemy, hex_from_offset(0, 1))
            .hp(0).summoner(actor.entity).ability_names(&[]).build();
        let actor_id = actor.entity;

        let summon_def = AbilityDef {
            id: AbilityId::from("summon_spirit"),
            name: "summon_spirit".into(),
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template: "spirit".into(),
                max_active: Some(2),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };

        let mut content = empty_content();
        content.abilities.insert(summon_def.id.clone(), summon_def.clone());
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, enemy, alive, dead], 1);
        let maps = empty_maps();
        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        let has_summon = plans.iter().any(|p| {
            p.steps.iter().any(|s| matches!(
                s, PlanStep::Cast { ability, .. } if *ability == summon_def.id
            ))
        });
        assert!(
            has_summon,
            "dead summons must not occupy cap slots — summon must still be planned",
        );
    }

    /// Multi-step plans must also respect cap: with cap=1 and 0 live summons,
    /// at most ONE summon cast per plan — sim.apply_step doesn't materialise
    /// the summon, so the second step must be pruned by the plan-level count.
    #[test]
    fn generate_plans_caps_multiple_summons_within_single_plan() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).ap(3).ability_names(&["summon_spirit"]).build();
        let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
        let actor_id = actor.entity;

        let summon_def = AbilityDef {
            id: AbilityId::from("summon_spirit"),
            name: "summon_spirit".into(),
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template: "spirit".into(),
                max_active: Some(1),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        };

        let mut content = empty_content();
        content.abilities.insert(summon_def.id.clone(), summon_def.clone());
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 3;
        let ctx = make_ctx(&content, &difficulty);

        let snap = BattleSnapshot::new(vec![actor, enemy], 1);
        let maps = empty_maps();
        let plans = generate_plans(actor_id, &ctx, &snap, &maps);

        for p in &plans {
            let summons = p.steps.iter().filter(|s| matches!(
                s, PlanStep::Cast { ability, .. } if *ability == summon_def.id
            )).count();
            assert!(
                summons <= 1,
                "plan stacked {summons} summon casts with cap=1: {:?}",
                p.steps,
            );
        }
    }

    /// Helper: total cast damage across every Cast step in every plan.
    fn cast_damage_sum(plans: &[TurnPlan]) -> f32 {
        plans
            .iter()
            .flat_map(|p| p.outcomes.iter().zip(p.steps.iter()))
            .filter(|(_, s)| matches!(s, PlanStep::Cast { .. }))
            .map(|(o, _)| o.damage)
            .sum()
    }

}
