//! Plan scoring: registry-driven aggregation of 10 factors over `StepFactor`
//! and `PlanFactor` enums, then batch-normalised and weighted per role axis.
//!
//! ## Factor aggregation (post-8.A)
//!
//! Step factors (`StepFactor` enum, 7 variants): discounted sum across Cast steps.
//! Each `StepFactor::f.compute(ctx, step, outcome, needs)` returns the raw value;
//! step[k] is weighted by `base_discount^k` (0.75 easy / 0.85 normal / 0.90 hard).
//!
//! Plan factors (`PlanFactor` enum, 3 variants): plan-terminal, computed once per plan.
//! - `Intent`: discounted sum of `intent_score` across all steps (see post-goal note).
//! - `TempoGain`: `compute_plan_tempo_gain(plan, intent, ctx)`.
//! - `SelfSurvival`: `compute_plan_self_survival(plan, ctx)`.
//!
//! Terminal factors (`TerminalFactor` enum, 8 variants): separate `terminal_state_score`
//! pass, weighted by `axis_terminal_weights` × `NeedSignals` modulation.
//!
//! **Post-goal behavior**: once a step kills the current `FocusTarget`/`ApplyCC`
//! target, subsequent steps skip the intent aggregation. All other factors continue
//! at normal geometric decay.
//!
//! Signed factors (can be negative): `Scarcity`, `Saturation` (step), `Intent`,
//! `TempoGain` (plan). These use symmetric normalisation ÷ max(|min|, |max|).
//! Non-signed factors use max normalisation → [0, 1].
//!
//! Picking jitter (deterministic noise) is applied in `PickBestStage`, not here.

use crate::combat::ai::scoring::factors::{
    compute_plan_self_survival, compute_plan_tempo_gain,
    plan as plan_factors, step as step_factors,
    BatchStats, PlanFactor, PlanFactorValues, ScoredStep, StepFactor, TerminalFactor,
    default_norm,
};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::intent::{cc_reach, evaluate_last_stand_step, intent_score, pursuit_move_score, TacticalIntent};
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::scoring::factors::terminal_state::terminal_state_score;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::estimate_st_damage;
use crate::combat::ai::orchestration::{AiWorld, ScoringCtx};
use crate::content::abilities::{CasterContext, EffectDef};
#[cfg(test)]
use crate::content::abilities::EffectCalcExt;
use combat_engine::modifier;
use crate::game::components::Abilities;
use bevy::prelude::Entity;

/// Per-factor contribution used both in `aggregate_factors_to_score` (Pass 1) and in
/// `PickBestStage` (step 11.4 additive composition).
///
/// Returns `default_norm(raw_value, stats, signed) × weight` — the exact
/// quantity that `aggregate_factors_to_score` adds to a plan's score for one factor.
/// Exposing it here keeps both callsites mathematically identical.
pub fn factor_contribution(raw_value: f32, stats: &BatchStats, signed: bool, weight: f32) -> f32 {
    default_norm(raw_value, stats, signed) * weight
}

/// Worst danger value across the plan's path tiles + its final tile.
/// Excludes the actor's starting tile — callers that care about it (the
/// scorer's risk factor) fold it in on top. Sanity uses this directly
/// because it tracks `current_danger` (the start) through a separate
/// signal. Single source of truth for "how exposed does this plan get
/// while traversing".
///
/// # Overlap note (5.5)
/// `worst_path_danger` ≠ `terminal::exposure_at_end`: this function returns
/// `max(danger[all_path_tiles ∪ final_pos])` — the worst exposure *along the
/// entire route*. `exposure_at_end` returns only `danger[final_pos]`. A plan
/// can cross a dangerous tile and land safely; `worst_path_danger` catches
/// the transit risk while `exposure_at_end` ignores it. Both are used: sanity
/// uses `worst_path_danger` to penalise risky traversal for low-HP actors;
/// the terminal aggregator uses `exposure_at_end` to penalise unsafe resting
/// positions. Keep both — they answer different questions.
pub fn worst_path_danger(plan: &TurnPlan, maps: &InfluenceMaps) -> f32 {
    let mut max_d = maps.danger.get(plan.final_pos);
    for step in &plan.steps {
        if let PlanStep::Move { path } = step {
            for &h in path {
                let d = maps.danger.get(h);
                if d > max_d {
                    max_d = d;
                }
            }
        }
    }
    max_d
}

/// Top-level entry. Produces one composite score per plan plus the raw
/// pre-normalization factor matrix (so log writers / offline tools can
/// recalibrate weights without rerunning sim).
pub fn score_plans_with_raw(
    plans: &mut [TurnPlan],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> (Vec<f32>, Vec<PlanFactorValues>) {
    if plans.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let raw: Vec<PlanFactorValues> = plans
        .iter()
        .map(|p| compute_plan_factors(p, intent, ctx))
        .collect();
    let scores = aggregate_factors_to_score(plans, &raw, ctx);
    (scores, raw)
}

/// Recompute scores under a **new** intent without re-running the
/// intent-independent factor computation. The caller hands in the raw matrix
/// produced by an earlier `score_plans_with_raw`; we only overwrite the
/// intent column (`factor[7]`) per plan and re-finalize. Used by the utility
/// pipeline's viability-fallback branch — every plan rescored under the
/// same intent.
pub fn rescore_with_intent(
    plans: &mut [TurnPlan],
    raw: &mut [PlanFactorValues],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Vec<f32> {
    for (p, f) in plans.iter().zip(raw.iter_mut()) {
        f.set_plan(PlanFactor::Intent, compute_plan_intent_sum(p, intent, ctx, EvaluationMode::Default));
        f.set_plan(PlanFactor::TempoGain, compute_plan_tempo_gain(p, intent, ctx));
    }
    aggregate_factors_to_score(plans, raw, ctx)
}

/// Recompute scores with **per-plan** evaluation modes. Each plan's
/// intent-column is computed under `modes[i].effective_intent(global)` —
/// plans with `mode=Default` use `global`, plans with `mode=LastStand`
/// always score under `TacticalIntent::LastStand` regardless of `global`.
///
/// Used by the ADAPTATION layer, which flags per-plan overrides
/// (`ExpectedSelfLethal`) without altering other plans' evaluation. Global
/// normalisation in `aggregate_factors_to_score` runs once across the mixed
/// intent-column values, so adapted and non-adapted plans remain
/// comparable in a single batch-normalised space.
///
/// Preconditions:
/// - `modes.len() == plans.len() == raw.len()`. Asserted in debug;
///   production fails soft by iterating the shorter of the two.
pub fn rescore_with_per_plan_modes(
    plans: &mut [TurnPlan],
    raw: &mut [PlanFactorValues],
    modes: &[EvaluationMode],
    global: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Vec<f32> {
    debug_assert_eq!(plans.len(), raw.len());
    debug_assert_eq!(plans.len(), modes.len());
    for ((p, f), mode) in plans.iter().zip(raw.iter_mut()).zip(modes.iter()) {
        // Pass the mode directly: when LastStand, compute_plan_intent_sum routes
        // to evaluate_last_stand_step; for Default it uses the global intent.
        f.set_plan(PlanFactor::Intent, compute_plan_intent_sum(p, global, ctx, *mode));
        f.set_plan(PlanFactor::TempoGain, compute_plan_tempo_gain(p, global, ctx));
    }
    aggregate_factors_to_score(plans, raw, ctx)
}

/// Batch-normalise raw factors, apply role weights + difficulty multipliers.
/// Returns pre-modifier, pre-noise scores. `PlanModifiersStage` добавляет
/// modifiers; `PickBestStage` добавляет jitter.
pub fn aggregate_factors_to_score(
    plans: &mut [TurnPlan],
    raw: &[PlanFactorValues],
    ctx: &ScoringCtx,
) -> Vec<f32> {
    let active = ctx.active;
    let snap = ctx.snap;
    let world = ctx.world;

    // Per-factor min/max for batch-relative normalization via registry walk.
    // BatchStats indexed by StepFactor::iter() then PlanFactor::iter() order,
    // matching PlanFactorValues layout.
    const NFACTORS: usize = step_factors::COUNT + plan_factors::COUNT;
    let mut stats = [BatchStats { min: 0.0, max: 0.0 }; NFACTORS];
    for factors in raw {
        for f in StepFactor::iter() {
            let v = factors.get(f);
            let s = &mut stats[f as usize];
            if v > s.max { s.max = v; }
            if v < s.min { s.min = v; }
        }
        for f in PlanFactor::iter() {
            let idx = StepFactor::count() + f as usize;
            let v = factors.get_plan(f);
            let s = &mut stats[idx];
            if v > s.max { s.max = v; }
            if v < s.min { s.min = v; }
        }
    }

    // Step 6.4: use continuation evaluator weights when actor has a stored goal.
    // Only the role-axis aggregation changes; sanity-mask, intent/scarcity
    // modulation, and the repair-affinity bonus (6.3) are unchanged.
    let mut weights = if ctx.last_goal.is_some() {
        active.cache.role.factor_weights_continuation(world.tuning)
    } else {
        active.cache.role.factor_weights(world.tuning)
    };
    // Intent slot: StepFactor::count() + PlanFactor::Intent as usize = 7 + 0 = 7
    weights[StepFactor::count() + PlanFactor::Intent as usize] *= world.difficulty.intent_commitment;
    // Scarcity slot: StepFactor::Scarcity as usize = 5
    weights[StepFactor::Scarcity as usize] *= world.difficulty.resource_discipline;

    // Pass 1: compute noise-free scores via registry walk.
    let mut scores: Vec<f32> = raw
        .iter()
        .zip(plans.iter())
        .map(|(factors, _plan)| {
            let mut score = 0.0f32;
            for f in StepFactor::iter() {
                let i = f as usize;
                score += factor_contribution(factors.get(f), &stats[i], f.signed(), weights[i]);
            }
            for f in PlanFactor::iter() {
                let i = StepFactor::count() + f as usize;
                score += factor_contribution(factors.get_plan(f), &stats[i], f.signed(), weights[i]);
            }
            score
        })
        .collect();

    // Terminal annotation pass: populate plan.annotation.terminal per plan.
    for plan in plans.iter_mut() {
        plan.annotation.terminal = terminal_state_score(plan, snap, ctx);
    }

    // Terminal aggregation (step 5.4): add terminal-state contribution to
    // each plan's score via TerminalFactor registry walk.
    // Each axis weighted by role terminal weights × NeedAxis modulation.
    // NeedAxis::None.amplify(_) = 1.0 — preserves FP-exact reproduction of
    // line_actionability (slot 5) and pressure_spacing_zone (slot 7) which
    // have no NeedSignals multiplier in the legacy formula.
    {
        // Step 6.4: use continuation terminal weights when actor has a stored goal.
        let tw = if ctx.last_goal.is_some() {
            active.cache.role.terminal_weights_continuation(world.tuning)
        } else {
            active.cache.role.terminal_weights(world.tuning)
        };
        let needs = ctx.need_signals;
        for (plan, score) in plans.iter().zip(scores.iter_mut()) {
            let t = &plan.annotation.terminal;
            for f in TerminalFactor::iter() {
                *score += t.get(f) * tw[f as usize] * f.need_modulation().amplify(&needs);
            }
        }
    }

    scores
}

/// Walk the plan pool, gather unique `Summon` template ids, and price each
/// once via `estimate_st_damage`. Replaces a per-plan rebuild of
/// `CasterContext` + `Abilities` clone that previously fired inside the
/// per-plan scoring loop. Returns an empty map when no plan summons.
pub fn build_summon_dpr_cache(
    plans: &[TurnPlan],
    ctx: &AiWorld,
) -> std::collections::HashMap<String, f32> {
    use std::collections::HashMap;
    let mut cache: HashMap<String, f32> = HashMap::new();
    for plan in plans {
        for step in &plan.steps {
            let PlanStep::Cast { ability, .. } = step else { continue };
            let Some(def) = ctx.content.abilities.get(ability) else { continue };
            let EffectDef::Summon { template_id, .. } = &def.effect else { continue };
            if cache.contains_key(template_id) {
                continue;
            }
            let Some(tpl) = ctx.content.unit_templates.get(template_id) else {
                cache.insert(template_id.clone(), 0.0);
                continue;
            };
            let weapon = ctx.content.weapons.get(&tpl.equipment.main_hand);
            let caster_ctx = CasterContext {
                str_mod: modifier(tpl.stats.strength),
                int_mod: modifier(tpl.stats.intelligence),
                spell_power: weapon.map_or(0, |wd| wd.spell_power),
                weapon_dice: weapon.map(|wd| wd.dice),
            };
            let abilities = Abilities(tpl.ability_ids.clone());
            let dpr = estimate_st_damage(&caster_ctx, &abilities, ctx.content);
            cache.insert(template_id.clone(), dpr);
        }
    }
    cache
}

/// Compute the 10 raw utility factors for a single plan. Thin combinator over
/// `compute_plan_factors_sans_intent` + intent/tempo/self_survival columns —
/// kept so scorer tests and any single-shot caller that does want both halves
/// in one call have a stable entry point. See module docs for per-factor
/// aggregation rules.
pub fn compute_plan_factors(
    plan: &TurnPlan,
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> PlanFactorValues {
    let mut out = compute_plan_factors_sans_intent(plan, ctx);
    out.set_plan(PlanFactor::Intent, compute_plan_intent_sum(plan, intent, ctx, EvaluationMode::Default));
    out.set_plan(PlanFactor::TempoGain, compute_plan_tempo_gain(plan, intent, ctx));
    out
}

/// Everything except the intent, tempo_gain, and self_survival factors (they
/// stay 0.0). Intent-independent, so the utility pipeline computes this once
/// per plan and reuses it across viability / LastStand intent swaps.
pub fn compute_plan_factors_sans_intent(
    plan: &TurnPlan,
    ctx: &ScoringCtx,
) -> PlanFactorValues {
    let active = ctx.active;
    let snap = ctx.snap;
    // No sim is run here: the generator already produced the sim state after
    // every step and cached it on the plan. `pre_step_snapshot` handles both
    // the `idx == 0` baseline and the deserialized-plan case (empty
    // `sim_snapshots` because of `#[serde(skip)]`) by falling back to `snap`;
    // see `TurnPlan::sim_snapshots` shape invariant.
    debug_assert!(
        plan.sim_snapshots.is_empty() || plan.sim_snapshots.len() == plan.steps.len(),
        "TurnPlan sim_snapshots must align with steps, or be empty (deserialized)",
    );

    // sums[f as usize] for StepFactor variants, discounted per-step.
    let mut sums = [0.0f32; step_factors::COUNT];

    let base_discount = ctx.world.difficulty.plan_step_discount;
    let mut step_weight: f32 = 1.0;

    for (idx, step) in plan.steps.iter().enumerate() {
        let pre_snap = plan.pre_step_snapshot(idx, snap);
        let Some(sim_actor) = pre_snap.unit(active.entity()) else {
            break;
        };

        let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);

        if let PlanStep::Cast { .. } = step {
            // Mid-plan: shift perspective to the simulated actor + pre-step snap.
            // NOTE: StepFactor::Saturation::compute reads ctx.snap as pre-step
            // snapshot — correct only when called inside this with_perspective block.
            let step_ctx = ctx.with_perspective(sim_actor, pre_snap);
            let step_outcome = plan.annotation.outcomes.get(idx).cloned().unwrap_or_default();
            for f in StepFactor::iter() {
                let v = f.compute(&step_ctx, &scored_step, &step_outcome, &ctx.need_signals);
                sums[f as usize] += v * step_weight;
            }
        }

        step_weight *= base_discount;
    }

    let mut out = PlanFactorValues::default();
    for f in StepFactor::iter() {
        out.set(f, sums[f as usize]);
    }
    // plan-level factors: intent and tempo_gain filled in by compute_plan_factors;
    // self_survival computed here.
    out.set_plan(PlanFactor::SelfSurvival, compute_plan_self_survival(plan, ctx));
    out
}

/// Intent-column aggregation for a plan.
///
/// ## Pure-move plans under FocusTarget / ApplyCC (step-1b, Variant A)
///
/// When the plan contains **no Cast steps** and the intent is `FocusTarget` or
/// `ApplyCC`, path length must not generate intent credit — a 3-step round-trip
/// ending at the same tile as a 2-step plan should score identically. The fix:
/// replace `Σ intent_score(step) × discount^k` with a single
/// `pursuit_move_score(actor_start, plan.final_pos, target.pos, reach)` call
/// that scores the plan by where it actually ends up, not how many steps it took.
///
/// ## Cast plans under FocusTarget / ApplyCC: post-first-Cast tail (step-1c)
///
/// For plans of the shape `steps[0..cast_idx] · Cast · steps[cast_idx+1..]`
/// under `FocusTarget`/`ApplyCC` (excluding `ProtectSelf`):
///
/// - **Pre-Cast steps** (Move setup): per-step discounted sum, unchanged.
/// - **Cast step**: per-step intent_score with discount, unchanged.
/// - **Post-Cast tail**: collapsed into a single terminal-pursuit call
///   `pursuit_move_score(cast_pos, plan.final_pos, target.pos, reach)`
///   multiplied by `base_discount^(cast_idx + 1)`.
///
/// Rationale: the post-Cast tail is never physically executed (committed_decision
/// is only the prefix up to and including the Cast). Per-step Σ over the tail
/// inflates intent credit linearly with tail length — a round-trip tail (Cast
/// then retreat to cast_pos) still earned ~+0.58 INT per extra step, causing
/// phantom-tail plans to outscore shorter equivalents. The terminal shortcut
/// scores the tail by net displacement from cast_pos, which is zero for a
/// round-trip and positive only for genuine approach.
///
/// Multiple Casts: the shortcut applies after the **first** Cast. All subsequent
/// steps (Cast or Move) are collapsed into the single terminal-pursuit call.
/// The first Cast is the action commit boundary; beam search replans after it.
///
/// ProtectSelf is excluded: its per-step tile-safety semantics differ for each
/// intermediate position and cannot be reduced to a single terminal value.
///
/// ## Other intents / ProtectSelf
///
/// Per-step discounted sum is preserved for all non-pursuit intents and for
/// ProtectSelf. Cast-step intent values are semantically richer and do not
/// exhibit path-length inflation.
///
/// ## goal_achieved latch
///
/// Once a `FocusTarget`/`ApplyCC` target is killed by a Cast step, the latch
/// fires and the post-Cast tail contribution is set to zero — pursuit is
/// irrelevant when the goal is already solved.
pub fn compute_plan_intent_sum(
    plan: &TurnPlan,
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
    mode: EvaluationMode,
) -> f32 {
    // LastStand evaluation regime: bypass intent-specific routing, score every
    // step through evaluate_last_stand_step directly.
    if mode == EvaluationMode::LastStand {
        let base_discount = ctx.world.difficulty.plan_step_discount;
        let mut intent_sum = 0.0f32;
        let mut step_weight = 1.0f32;
        for (idx, step) in plan.steps.iter().enumerate() {
            let pre_snap = plan.pre_step_snapshot(idx, ctx.snap);
            let Some(sim_actor) = pre_snap.unit(ctx.active.entity()) else { break; };
            let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);
            let step_ctx = ctx.with_perspective(sim_actor, pre_snap);
            intent_sum += evaluate_last_stand_step(&scored_step, &step_ctx) * step_weight;
            step_weight *= base_discount;
        }
        return intent_sum;
    }

    // Flee evaluation regime: bypass intent-specific routing, score every
    // step through evaluate_flee_step directly.
    if mode == EvaluationMode::Flee {
        use crate::combat::ai::intent::score::evaluate_flee_step;
        let base_discount = ctx.world.difficulty.plan_step_discount;
        let mut intent_sum = 0.0f32;
        let mut step_weight = 1.0f32;
        for (idx, step) in plan.steps.iter().enumerate() {
            let pre_snap = plan.pre_step_snapshot(idx, ctx.snap);
            let Some(sim_actor) = pre_snap.unit(ctx.active.entity()) else { break; };
            let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);
            let step_ctx = ctx.with_perspective(sim_actor, pre_snap);
            intent_sum += evaluate_flee_step(&scored_step, &step_ctx) * step_weight;
            step_weight *= base_discount;
        }
        return intent_sum;
    }

    debug_assert!(
        plan.sim_snapshots.is_empty() || plan.sim_snapshots.len() == plan.steps.len(),
        "TurnPlan sim_snapshots must align with steps, or be empty (deserialized)",
    );

    let active = ctx.active;
    let snap = ctx.snap;
    let world = ctx.world;
    let content = world.content;
    let base_discount = world.difficulty.plan_step_discount;

    // Detect pure-move plan: no Cast step anywhere.
    let is_pure_move = plan.steps.iter().all(|s| matches!(s, PlanStep::Move { .. }));

    // Step-1b: for pure-move plans under pursuit intents, score by final
    // position only — path length must not be a source of intent credit.
    if is_pure_move {
        match intent {
            TacticalIntent::FocusTarget { target } => {
                return match snap.unit(*target) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(active.cache.max_attack_range);
                        pursuit_move_score(active.pos, plan.final_pos, t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            TacticalIntent::ApplyCC { target } => {
                return match snap.unit(*target) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(cc_reach(active, content));
                        pursuit_move_score(active.pos, plan.final_pos, t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Other intents: fall through to per-step accumulation below.
            _ => {}
        }
    }

    // Step-1c: post-first-Cast tail shortcut for FocusTarget/ApplyCC.
    // ProtectSelf is excluded — tile safety is position-specific each step.
    let applies_cast_shortcut = matches!(
        intent,
        TacticalIntent::FocusTarget { .. } | TacticalIntent::ApplyCC { .. }
    );

    // Per-step discounted accumulation for pre-Cast steps and the Cast itself.
    // Once the first Cast is processed, the post-Cast tail is collapsed into
    // a single terminal-pursuit call (step-1c) instead of continuing the loop.
    let mut step_weight: f32 = 1.0;
    let mut intent_sum = 0.0f32;
    let mut goal_achieved = false;

    for (idx, step) in plan.steps.iter().enumerate() {
        let pre_snap = plan.pre_step_snapshot(idx, snap);
        let Some(sim_actor) = pre_snap.unit(active.entity()) else {
            break;
        };
        let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);

        if !goal_achieved {
            let step_ctx = ctx.with_perspective(sim_actor, pre_snap);
            let step_outcome = plan.annotation.outcomes.get(idx).cloned().unwrap_or_default();
            let iv = intent_score(intent, &scored_step, &step_ctx, &step_outcome, EvaluationMode::Default);
            intent_sum += iv * step_weight;
        }

        step_weight *= base_discount;

        if !goal_achieved {
            let killed = plan
                .outcomes
                .get(idx)
                .map(|o| o.killed.as_slice())
                .unwrap_or(&[]);
            if killed_intent_target(killed, intent) {
                goal_achieved = true;
            }
        }

        // First Cast encountered: apply post-Cast terminal shortcut and stop.
        if applies_cast_shortcut && matches!(step, PlanStep::Cast { .. }) {
            // Post-Cast tail is empty or goal already reached → no tail credit.
            let has_tail = idx + 1 < plan.steps.len();
            if has_tail && !goal_achieved {
                let cast_pos = sim_actor.pos;
                let tail_discount = step_weight; // = base_discount^(cast_idx+1)
                let tail_score = match intent {
                    TacticalIntent::FocusTarget { target } => {
                        match snap.unit(*target) {
                            Some(t) => {
                                let reach = (active.speed.max(0) as u32)
                                    .saturating_add(active.cache.max_attack_range);
                                pursuit_move_score(cast_pos, plan.final_pos, t.pos, reach)
                            }
                            None => 0.0,
                        }
                    }
                    TacticalIntent::ApplyCC { target } => {
                        match snap.unit(*target) {
                            Some(t) => {
                                let reach = (active.speed.max(0) as u32)
                                    .saturating_add(cc_reach(active, content));
                                pursuit_move_score(cast_pos, plan.final_pos, t.pos, reach)
                            }
                            None => 0.0,
                        }
                    }
                    _ => 0.0,
                };
                intent_sum += tail_score * tail_discount;
            }
            break;
        }
    }

    intent_sum
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "aggregate_tests.rs"]
mod tests;
