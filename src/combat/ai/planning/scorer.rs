//! Plan scoring: replay each plan on a sim, aggregate 9 factors, normalize and
//! weight the same way single-candidate scoring does.
//!
//! Aggregation rules per factor:
//! - `damage`, `heal`, `cc`, `scarcity`: **discounted sum** across cast steps.
//!   step[k] contributes its per-step factor value weighted by
//!   `base_discount^k`, where `base_discount` is a difficulty knob (0.75 easy
//!   / 0.85 normal / 0.90 hard). Rationale: future steps carry execution
//!   uncertainty — each depth multiplies the chance of state drift between
//!   plan and reality. The discount also prevents "cheap-filler" extensions
//!   from winning the damage normalization race against genuinely strong
//!   short plans.
//! - **Post-goal aggressive discount**: once a step kills the current
//!   `FocusTarget`/`ApplyCC` target, remaining steps are additionally scaled
//!   by ×0.5. Post-kill heal/move actions still contribute (preserves info
//!   that Plan B does more than Plan A), but they're properly treated as
//!   bonuses rather than peers of the goal-achieving step.
//! - `kill`: max across steps (binary "did this plan kill anyone?"), not
//!   discounted — a goal outcome is valued at achievement magnitude.
//! - `focus`: max target_priority across casts, not discounted.
//! - `intent`: max intent_score across steps, not discounted. Moves
//!   participate so Reposition intent lands on the move step.
//! - `position`: `evaluate_position(final_pos)` — terminal.
//! - `risk`: `1 − max_danger_along_path` — worst tile the actor traverses or
//!   casts from.

#![allow(clippy::too_many_arguments)]

use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::factors::{self, NUM_FACTORS};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{intent_score, TacticalIntent};
use crate::combat::ai::planning::sim::SimState;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::UtilityContext;
use crate::core::DiceRng;
use bevy::prelude::Entity;

/// Factors with negative values use symmetric normalization (mirrors
/// `factors/mod.rs::SIGNED_FACTOR`). Position/intent/scarcity can go negative.
const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, true, false, false, true, true,
];

/// Top-level entry. Produces one composite score per plan using the same
/// normalization+weight+noise pipeline as `score_candidates`.
pub fn score_plans(
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> Vec<f32> {
    score_plans_with_raw(plans, active, intent, ctx, snap, maps, reservations, rng).0
}

/// Same computation as `score_plans`, but also returns the **pre-normalization**
/// raw factor matrix so log writers / offline tools can recalibrate weights
/// without rerunning sim.
pub fn score_plans_with_raw(
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> (Vec<f32>, Vec<[f32; NUM_FACTORS]>) {
    if plans.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let raw: Vec<[f32; NUM_FACTORS]> = plans
        .iter()
        .map(|p| compute_plan_factors(p, active, intent, ctx, snap, maps, reservations))
        .collect();

    // Per-factor min/max for batch-relative normalization.
    let mut maxes = [0.0f32; NUM_FACTORS];
    let mut mins = [0.0f32; NUM_FACTORS];
    for factors in &raw {
        for (i, &v) in factors.iter().enumerate() {
            if v > maxes[i] {
                maxes[i] = v;
            }
            if v < mins[i] {
                mins[i] = v;
            }
        }
    }
    let mut denom = [0.0f32; NUM_FACTORS];
    for i in 0..NUM_FACTORS {
        denom[i] = if SIGNED_FACTOR[i] {
            mins[i].abs().max(maxes[i].abs())
        } else {
            maxes[i]
        };
    }

    let mut weights = active.role.factor_weights();
    weights[7] *= ctx.difficulty.intent_commitment;
    weights[8] *= ctx.difficulty.resource_discipline;
    let noise_amp = ctx.difficulty.score_noise();

    let scores: Vec<f32> = raw
        .iter()
        .map(|factors| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    factors[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }
            if noise_amp > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
                score += noise;
            }
            score
        })
        .collect();
    (scores, raw)
}

/// Extra multiplicative discount on step_weight applied **after** a step that
/// kills the current intent's target. Expresses the scoring intuition that
/// post-goal actions are genuine bonuses but shouldn't be weighed as peers of
/// the goal-achieving step.
const POST_GOAL_DISCOUNT: f32 = 0.5;

/// Compute the 9 raw utility factors for a single plan. Empty plan (seed)
/// yields zeros for cumulative factors and baselines on position/risk at the
/// actor's current tile. See module docs for per-factor aggregation rules.
pub fn compute_plan_factors(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> [f32; NUM_FACTORS] {
    let mut sim = SimState::from_snapshot(snap, active.entity);

    let mut damage_sum = 0.0f32;
    let mut heal_sum = 0.0f32;
    let mut kill_max = 0.0f32;
    let mut cc_sum = 0.0f32;
    let mut scarcity_sum = 0.0f32;
    let mut focus_max = 0.0f32;
    let mut intent_max = f32::NEG_INFINITY;
    let mut path_danger_max = maps.danger.get(active.pos);

    let base_discount = ctx.difficulty.plan_step_discount;
    let mut step_weight: f32 = 1.0;
    let mut goal_achieved = false;

    for (idx, step) in plan.steps.iter().enumerate() {
        // Sim state *before* applying this step.
        let sim_actor = match sim.actor_unit() {
            Some(u) => u.clone(),
            None => break,
        };

        let candidate = match step {
            PlanStep::Cast { ability, target, target_pos } => ActionCandidate {
                tile: sim_actor.pos,
                path: Vec::new(),
                kind: CandidateKind::Cast {
                    ability: ability.clone(),
                    target_pos: *target_pos,
                    target: Some(*target),
                },
            },
            PlanStep::Move { path } => {
                for &h in path {
                    let d = maps.danger.get(h);
                    if d > path_danger_max {
                        path_danger_max = d;
                    }
                }
                let dest = *path.last().unwrap_or(&sim_actor.pos);
                ActionCandidate {
                    tile: dest,
                    path: path.clone(),
                    kind: CandidateKind::MoveOnly,
                }
            }
        };

        // Intent factor participates uniformly across Cast and Move steps —
        // taken as the max, so it's not scaled by step_weight.
        let iv = intent_score(
            intent,
            &candidate,
            &sim_actor,
            &sim.snapshot,
            maps,
            ctx.content,
            ctx.difficulty,
        );
        if iv > intent_max {
            intent_max = iv;
        }

        if let PlanStep::Cast { .. } = step {
            let raw = factors::compute_factors(
                &candidate,
                &sim_actor,
                intent,
                ctx,
                &sim.snapshot,
                maps,
                reservations,
            );
            // Discounted cumulative factors.
            damage_sum += raw[0] * step_weight;
            cc_sum += raw[2] * step_weight;
            heal_sum += raw[3] * step_weight;
            scarcity_sum += raw[8] * step_weight;
            // Un-discounted outcome/priority signals.
            if raw[1] > kill_max {
                kill_max = raw[1];
            }
            if raw[6] > focus_max {
                focus_max = raw[6];
            }
        }

        sim.apply_step(step, ctx.caster, ctx.content);

        // Geometric per-step discount on the next step's contribution.
        step_weight *= base_discount;

        // Post-goal aggressive discount fires at most once, when this step
        // killed the current intent's declared target. The kill signal comes
        // from the sim — AoE that incidentally kills the intent target
        // triggers the bump just like a direct cast.
        if !goal_achieved {
            let killed = plan
                .outcomes
                .get(idx)
                .map(|o| o.killed.as_slice())
                .unwrap_or(&[]);
            if killed_intent_target(killed, intent) {
                step_weight *= POST_GOAL_DISCOUNT;
                goal_achieved = true;
            }
        }
    }

    let position = evaluate_position(plan.final_pos, &active.role, maps);
    let final_danger = path_danger_max.max(maps.danger.get(plan.final_pos));
    let risk = 1.0 - final_danger;

    // Focus floor for empty plans: use the best priority target on current
    // snapshot so "do nothing" doesn't misleadingly score with focus=0.
    if plan.steps.is_empty() {
        focus_max = snap
            .enemies_of(active.team)
            .map(|t| target_priority(active, t, snap))
            .fold(0.0f32, f32::max);
    }

    let intent_val = if intent_max.is_finite() { intent_max } else { 0.0 };

    [
        damage_sum,
        kill_max,
        cc_sum,
        heal_sum,
        position,
        risk,
        focus_max,
        intent_val,
        scarcity_sum,
    ]
}

/// True iff the sim's step kills contain the intent's declared target. Only
/// `FocusTarget` and `ApplyCC` carry an explicit kill/CC goal; other intents
/// return false (they don't have a single "achievement" target).
fn killed_intent_target(killed: &[Entity], intent: &TacticalIntent) -> bool {
    let target = match intent {
        TacticalIntent::FocusTarget { target } => *target,
        TacticalIntent::ApplyCC { target } => *target,
        _ => return false,
    };
    killed.contains(&target)
}
