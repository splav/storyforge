//! Plan scoring: replay each plan on a sim, aggregate 10 factors, normalize and
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
//! - **Post-goal behavior**: once a step kills the current
//!   `FocusTarget`/`ApplyCC` target, the intent is satisfied. Subsequent
//!   steps skip the **intent** aggregation — they aren't aligned or
//!   misaligned, they're orthogonal to a now-solved goal. All other
//!   factors (damage, heal, cc, kill, scarcity) continue at their
//!   normal geometric `base^k` decay. No extra multiplier — post-goal
//!   actions are scored on their own merit, neither penalised as
//!   "bonuses" nor inflated as "peers".
//! - `kill`: **discounted sum** of `raw_kill × step_weight` across Cast
//!   steps. Accumulates count of planned kills (each `raw_kill` is
//!   binary 0/1 from `single_target_kill`) with geometric decay — a
//!   plan killing two enemies outscores one killing one.
//! - `intent`: **discounted sum** of `intent_score × step_weight`
//!   across all steps (Cast and Move). Captures alignment across the
//!   whole plan, including misalign penalties on tail steps that do
//!   drag the signal down. Skipped once the intent's goal is achieved
//!   (see post-goal above).
//! - `tempo_gain`: plan-terminal — captures approach quality + exit-danger
//!   of the full plan path. See `factors::tempo`.
//! - `self_survival`: plan-terminal — defensive value (heal + armor-buff +
//!   exit-AoO). See `factors::survival`.
//!
//! Phase 6 removed `position`, `risk`, and `focus` axes. Their signals are
//! now covered by `tempo_gain` and `self_survival`.

use crate::combat::ai::factors::{
    self, buff_saturation_penalty, compute_plan_self_survival, compute_plan_tempo_gain,
    PlanFactors, ScoredStep, INTENT_IDX, NUM_FACTORS, SCARCITY_IDX, SIGNED_FACTOR,
};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{cc_reach, intent_score, pursuit_move_score, TacticalIntent};
use crate::combat::ai::planning::adaptation::EvaluationMode;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::estimate_st_damage;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::trade::{trade_delta, trade_score, unit_value};
use crate::combat::ai::utility::{AiWorld, ScoringCtx};
use crate::content::abilities::{CasterContext, EffectDef};
use crate::core::modifier;
use crate::game::components::Abilities;
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use std::hash::{Hash, Hasher};

/// Worst danger value across the plan's path tiles + its final tile.
/// Excludes the actor's starting tile — callers that care about it (the
/// scorer's risk factor) fold it in on top. Sanity uses this directly
/// because it tracks `current_danger` (the start) through a separate
/// signal. Single source of truth for "how exposed does this plan get
/// while traversing".
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
    plans: &[TurnPlan],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> (Vec<f32>, Vec<PlanFactors>) {
    if plans.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let raw: Vec<PlanFactors> = plans
        .iter()
        .map(|p| compute_plan_factors(p, intent, ctx))
        .collect();
    let scores = finalize_scores(plans, &raw, ctx);
    (scores, raw)
}

/// Recompute scores under a **new** intent without re-running the
/// intent-independent factor computation. The caller hands in the raw matrix
/// produced by an earlier `score_plans_with_raw`; we only overwrite the
/// intent column (`factor[7]`) per plan and re-finalize. Used by the utility
/// pipeline's viability-fallback branch — every plan rescored under the
/// same intent.
pub fn rescore_with_intent(
    plans: &[TurnPlan],
    raw: &mut [PlanFactors],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Vec<f32> {
    for (p, f) in plans.iter().zip(raw.iter_mut()) {
        f.intent = compute_plan_intent_sum(p, intent, ctx);
        f.tempo_gain = compute_plan_tempo_gain(p, intent, ctx);
    }
    finalize_scores(plans, raw, ctx)
}

/// Recompute scores with **per-plan** evaluation modes. Each plan's
/// intent-column is computed under `modes[i].effective_intent(global)` —
/// plans with `mode=Default` use `global`, plans with `mode=LastStand`
/// always score under `TacticalIntent::LastStand` regardless of `global`.
///
/// Used by the ADAPTATION layer, which flags per-plan overrides
/// (`ExpectedSelfLethal`) without altering other plans' evaluation. Global
/// normalisation in `finalize_scores` runs once across the mixed
/// intent-column values, so adapted and non-adapted plans remain
/// comparable in a single batch-normalised space.
///
/// Preconditions:
/// - `modes.len() == plans.len() == raw.len()`. Asserted in debug;
///   production fails soft by iterating the shorter of the two.
pub fn rescore_with_per_plan_modes(
    plans: &[TurnPlan],
    raw: &mut [PlanFactors],
    modes: &[EvaluationMode],
    global: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Vec<f32> {
    debug_assert_eq!(plans.len(), raw.len());
    debug_assert_eq!(plans.len(), modes.len());
    for ((p, f), mode) in plans.iter().zip(raw.iter_mut()).zip(modes.iter()) {
        let effective = mode.effective_intent(*global);
        f.intent = compute_plan_intent_sum(p, &effective, ctx);
        f.tempo_gain = compute_plan_tempo_gain(p, &effective, ctx);
    }
    finalize_scores(plans, raw, ctx)
}

/// Batch-normalise raw factors, apply role weights + difficulty multipliers,
/// add summon bonus and score noise. Pure output — does not mutate `raw`.
///
/// Noise is **deterministic per plan**, not RNG-driven:
/// `hash((round, actor_entity, plan.canonical_key)) → noise ∈ [-1, 1]`.
/// This makes the pipeline reproducible across plan-pool permutations (e.g.
/// `dedup_by_logical_key`'s HashMap iteration order, or any future reorder
/// in generator). The old `rng.roll_d(1000)` scheme bound the Nth plan to
/// the Nth roll, so a reshuffle leaked a different noise vector even under
/// the same seed.
///
/// Amplitude is scaled by the pre-noise score spread (`max − min`), so on a
/// flat batch noise barely moves the ranking, while on a high-variance batch
/// it stays proportional. The old absolute-amplitude scheme made noise "loud"
/// when scores clustered and "quiet" when they spread.
pub fn finalize_scores(
    plans: &[TurnPlan],
    raw: &[PlanFactors],
    ctx: &ScoringCtx,
) -> Vec<f32> {
    let active = ctx.active;
    let snap = ctx.snap;
    let world = ctx.world;
    // Per-template summon DPR cache, computed once over the unique templates
    // referenced by Summon casts in this batch. Pre-cache: each Summon-step
    // per plan rebuilt CasterContext + walked the template's full ability set
    // (estimate_st_damage is O(K)). With M plans × S summons × K abilities
    // this scales as O(M·S·K); the cache replaces it with O(unique_templates·K)
    // upfront + O(1) lookup inside `plan_summon_bonus`.
    let summon_dpr = build_summon_dpr_cache(plans, world);
    // Actor's own `unit_value` is plan-independent — compute once per
    // batch and reuse as the tanh denominator inside `plan_trade_bonus`.
    let actor_value = unit_value(active, world.content);
    // Per-factor min/max for batch-relative normalization. Convert each
    // PlanFactors row to its array view once for the inner loop.
    let mut maxes = [0.0f32; NUM_FACTORS];
    let mut mins = [0.0f32; NUM_FACTORS];
    for factors in raw {
        for (i, v) in factors.as_array().into_iter().enumerate() {
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
    weights[INTENT_IDX] *= world.difficulty.intent_commitment;
    weights[SCARCITY_IDX] *= world.difficulty.resource_discipline;
    let noise_amp = world.difficulty.score_noise();

    // Pass 1: compute noise-free scores.
    let mut scores: Vec<f32> = raw
        .iter()
        .zip(plans.iter())
        .map(|(factors, plan)| {
            let arr = factors.as_array();
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    arr[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }
            // Summon bonus bypasses normalisation: the factor pipeline can't
            // see the strategic value of creating an ally, and for hybrid
            // roles the damage-axis weight is too low to lift a raw summon
            // score on its own.
            score += plan_summon_bonus(plan, active, world, snap, &summon_dpr);
            // Trade bonus: signed plan-level modifier in [-1, 1] via tanh
            // over `trade_delta / unit_value(self)`. Applied outside the
            // factor normalisation for the same reason as summon_bonus —
            // the factor pipeline has no channel for "is this exchange
            // worth it"; it answers "was anything useful done".
            score += plan_trade_bonus(plan, active, snap, world, actor_value);
            score
        })
        .collect();

    // Pass 2: add deterministic, batch-scaled noise.
    if noise_amp > 0.0 && !scores.is_empty() {
        let (s_min, s_max) = scores
            .iter()
            .copied()
            .filter(|s| s.is_finite())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
                (lo.min(s), hi.max(s))
            });
        // Amplitude floor: if every score is ±inf or spread is 0, fall back to
        // a small constant scale so noise still breaks exact ties. 0.05 matches
        // the `window` floor used downstream in `pick_best_plan`.
        let spread = if s_min.is_finite() && s_max.is_finite() {
            (s_max - s_min).max(0.05)
        } else {
            0.05
        };
        let effective_amp = noise_amp * spread;
        for (plan, score) in plans.iter().zip(scores.iter_mut()) {
            if !score.is_finite() {
                continue;
            }
            *score += plan_noise(plan, snap.round, active.entity, effective_amp);
        }
    }

    scores
}

/// Deterministic per-plan noise ∈ [−amp, +amp). Seed = hash((round, actor,
/// plan canonical key)) — order-invariant across any permutation of the plan
/// pool. `fxhash`-style finalizer maps the 64-bit hash into a uniform float;
/// the high bits are used because `DefaultHasher`'s low bits aren't stellar.
fn plan_noise(plan: &TurnPlan, round: u32, actor: Entity, amp: f32) -> f32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    round.hash(&mut h);
    actor.hash(&mut h);
    // `hash_canonical` needs a `start` tile; the actor's current position is
    // the same reference point `generator::dedup_by_logical_key` uses when
    // computing `logical_key`, so the seed is stable across the scoring /
    // dedup boundary.
    plan.hash_canonical(plan_start_tile(plan), &mut h);
    let bits = h.finish();
    // Take the top 24 bits → f32 mantissa precision, uniform in [0, 1).
    let u = ((bits >> 40) as u32) as f32 / (1u32 << 24) as f32;
    (u * 2.0 - 1.0) * amp
}

/// `walk_with_caster` needs the actor's starting tile. At scoring time we
/// don't keep the original start around per plan, but the generator always
/// emits plans with `sim_snapshots[0]` being the post-step-0 state and the
/// actor hasn't moved before step 0. For scoring-noise purposes any stable
/// choice works — we just need the same tile every time we rescore the same
/// plan. `plan.final_pos` is wrong (post-plan), so use the first Move step's
/// origin if any, else `final_pos`. Cheap, stable, and agrees with itself
/// across rescores.
fn plan_start_tile(plan: &TurnPlan) -> Hex {
    // The canonical-key hasher is self-consistent under any fixed start tile:
    // two identical plans always hash the same way, different plans hash
    // differently. We just need *a* stable tile. `final_pos` is the cheapest.
    plan.final_pos
}

/// Additive post-normalisation bonus for every `Summon` cast in the plan.
/// Each summon contributes `summon_dpr × cap_decay × saturation_mult`, where:
/// - `cap_decay = 1 − count/cap` — per-step cap pressure (local to one summoner).
/// - `saturation_mult = 0.65^total_allies` — global over-saturation penalty:
///   plans that summon into an already-full friendly roster score proportionally
///   less even before the cap math clips them, preventing "spam summons" from
///   dominating when the battlefield is already crowded.
///
/// Zero for plans without any summon casts. `summon_dpr` is the precomputed
/// per-template DPR table built once by `build_summon_dpr_cache`.
fn plan_summon_bonus(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    ctx: &AiWorld,
    snap: &BattleSnapshot,
    summon_dpr: &std::collections::HashMap<String, f32>,
) -> f32 {
    // Only LIVE summons occupy a cap slot (spawn.rs filters Dead too). Dead
    // units stay in the snapshot with hp=0 — counting them would make the AI
    // think the cap is reached when the spawn side would happily summon more.
    let mut count = snap
        .units
        .iter()
        .filter(|u| u.summoner == Some(active.entity) && u.is_alive())
        .count() as f32;

    // Global saturation: total live allies on the actor's team (excluding actor).
    let total_allies = snap
        .units
        .iter()
        .filter(|u| u.team == active.team && u.entity != active.entity && u.is_alive())
        .count() as f32;
    let saturation_mult = 0.65_f32.powf(total_allies);

    let mut total = 0.0f32;
    for step in &plan.steps {
        let PlanStep::Cast { ability, .. } = step else { continue };
        let Some(def) = ctx.content.abilities.get(ability) else { continue };
        let EffectDef::Summon { template, max_active } = &def.effect else { continue };

        let cap = max_active.unwrap_or(3).max(1) as f32;
        let decay = (1.0 - (count / cap)).max(0.0);
        if decay <= 0.0 {
            continue;
        }

        let dpr = summon_dpr.get(template).copied().unwrap_or(0.0);
        total += dpr * decay * saturation_mult;
        count += 1.0;
    }
    total
}

/// Plan-level signed modifier in roughly `[-TRADE_WEIGHT, +TRADE_WEIGHT]`.
///
/// Thin wrapper over `trade::trade_score` that first computes the
/// plan's trade breakdown. Kept here rather than inlined at the call
/// site so the log writer can reuse the same breakdown + score helper
/// without duplicating the formula — see `combat::ai::trade`.
fn plan_trade_bonus(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    ctx: &AiWorld,
    actor_value: f32,
) -> f32 {
    let br = trade_delta(plan, active, snap, ctx.content);
    trade_score(&br, actor_value)
}

/// Walk the plan pool, gather unique `Summon` template ids, and price each
/// once via `estimate_st_damage`. Replaces a per-plan rebuild of
/// `CasterContext` + `Abilities` clone that previously fired inside the
/// per-plan scoring loop. Returns an empty map when no plan summons.
fn build_summon_dpr_cache(
    plans: &[TurnPlan],
    ctx: &AiWorld,
) -> std::collections::HashMap<String, f32> {
    use std::collections::HashMap;
    let mut cache: HashMap<String, f32> = HashMap::new();
    for plan in plans {
        for step in &plan.steps {
            let PlanStep::Cast { ability, .. } = step else { continue };
            let Some(def) = ctx.content.abilities.get(ability) else { continue };
            let EffectDef::Summon { template, .. } = &def.effect else { continue };
            if cache.contains_key(template) {
                continue;
            }
            let Some(tpl) = ctx.content.unit_templates.get(template) else {
                cache.insert(template.clone(), 0.0);
                continue;
            };
            let weapon = ctx.content.weapons.get(&tpl.equipment.main_hand);
            let caster_ctx = CasterContext {
                str_mod: modifier(tpl.stats.strength),
                int_mod: modifier(tpl.stats.intelligence),
                spell_power: weapon.map_or(0, |wd| wd.spell_power),
                weapon_dice: weapon.map(|wd| wd.dice.clone()),
            };
            let abilities = Abilities(tpl.ability_ids.clone());
            let dpr = estimate_st_damage(&caster_ctx, &abilities, ctx.content);
            cache.insert(template.clone(), dpr);
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
) -> PlanFactors {
    let mut out = compute_plan_factors_sans_intent(plan, ctx);
    out.intent = compute_plan_intent_sum(plan, intent, ctx);
    out.tempo_gain = compute_plan_tempo_gain(plan, intent, ctx);
    out
}

/// Everything except the intent, tempo_gain, and self_survival factors (they
/// stay 0.0). Intent-independent, so the utility pipeline computes this once
/// per plan and reuses it across viability / LastStand intent swaps.
pub fn compute_plan_factors_sans_intent(
    plan: &TurnPlan,
    ctx: &ScoringCtx,
) -> PlanFactors {
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

    let mut damage_sum = 0.0f32;
    let mut heal_sum = 0.0f32;
    let mut kill_now_sum = 0.0f32;
    let mut kill_promised_sum = 0.0f32;
    let mut cc_sum = 0.0f32;
    let mut scarcity_sum = 0.0f32;
    let mut saturation_sum = 0.0f32;

    let base_discount = ctx.world.difficulty.plan_step_discount;
    let mut step_weight: f32 = 1.0;

    for (idx, step) in plan.steps.iter().enumerate() {
        let pre_snap = plan.pre_step_snapshot(idx, snap);
        let Some(sim_actor) = pre_snap.unit(active.entity).cloned() else {
            break;
        };

        let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);

        if let PlanStep::Cast { .. } = step {
            // Mid-plan: shift perspective to the simulated actor + pre-step snap.
            let step_ctx = ctx.with_perspective(&sim_actor, pre_snap);
            let raw = factors::compute_factors(&step_ctx, &scored_step);
            // Every Cast-accumulating factor uses the same shape: discounted
            // sum with base^k decay. Deep Casts keep contributing but weigh
            // less, reflecting execution uncertainty over plan depth.
            damage_sum += raw.damage * step_weight;
            kill_now_sum += raw.kill_now * step_weight;
            kill_promised_sum += raw.kill_promised * step_weight;
            cc_sum += raw.cc * step_weight;
            heal_sum += raw.heal * step_weight;
            scarcity_sum += raw.scarcity * step_weight;
            if let PlanStep::Cast { ability, target, .. } = step {
                let sat = buff_saturation_penalty(
                    ability, *target, active.entity, pre_snap, step_ctx.world.content,
                );
                saturation_sum += sat * step_weight;
            }
        }

        step_weight *= base_discount;
    }

    PlanFactors {
        damage: damage_sum,
        kill_now: kill_now_sum,
        kill_promised: kill_promised_sum,
        cc: cc_sum,
        heal: heal_sum,
        intent: 0.0,        // filled in by `compute_plan_intent_sum` when needed
        scarcity: scarcity_sum,
        tempo_gain: 0.0,    // filled in by `compute_plan_tempo_gain` when needed
        saturation: saturation_sum,
        self_survival: compute_plan_self_survival(plan, ctx),
    }
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
) -> f32 {
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
                            .saturating_add(active.max_attack_range);
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
        let Some(sim_actor) = pre_snap.unit(active.entity).cloned() else {
            break;
        };
        let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);

        if !goal_achieved {
            let step_ctx = ctx.with_perspective(&sim_actor, pre_snap);
            let iv = intent_score(intent, &scored_step, &step_ctx);
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
                                    .saturating_add(active.max_attack_range);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::planning::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::AiTags;
    use crate::combat::ai::test_helpers::make_scoring_ctx;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};

    /// Scorer-suite defaults: AP=2 (enough for a 1-AP cast), melee bruiser
    /// with one `melee_attack` ability. Mirrors the pre-builder factory.
    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        crate::combat::ai::test_helpers::UnitBuilder::new(id, team, pos)
            .ap(2)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .build()
    }

    use crate::combat::ai::test_helpers::empty_maps;

    fn test_ctx<'a>(
        content: &'a crate::content::content_view::ContentView,
        difficulty: &'a DifficultyProfile,
    ) -> AiWorld<'a> {
        crate::combat::ai::test_helpers::make_test_ctx(content, difficulty)
    }

    /// Pins the `intent` factor aggregation across single- and multi-cast plans
    /// under `FocusTarget`.
    ///
    /// **Step-1c semantics**: the post-first-Cast tail shortcut applies when
    /// intent is `FocusTarget`/`ApplyCC`. For a multi-cast plan
    /// `[Cast@focus, Cast@focus]`, only the first Cast contributes per-step
    /// intent; the second Cast is treated as the post-Cast tail and replaced by
    /// a single `pursuit_move_score(cast_pos, final_pos, focus.pos, reach)`
    /// call multiplied by `base_discount^1`. This is intentional — the second
    /// Cast is never physically executed (committed_decision is the first
    /// Cast prefix), so scoring it per-step inflates intent linearly with
    /// phantom tail length.
    ///
    /// Concrete formula for a `[Cast, Cast]` plan with `final_pos = actor.pos`:
    ///   intent = s1 + pursuit_move_score(cast_pos, final_pos, focus.pos, reach) × 0.85
    /// where `s1` is the per-step intent of the first Cast.
    ///
    /// Pure Move-preceded chains under FocusTarget are not pinned here —
    /// those are covered by `pure_move_chain_intent_equals_single_pursuit`.
    #[test]
    fn sum_factors_scale_by_step_weight() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        // Give actor a weapon die so melee_attack (weapon_attack effect)
        // produces non-zero damage factors. Without weapon_dice the caster_ctx
        // returns 0 expected damage, making the FocusTarget dot-product 0.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let focus = unit(2, Team::Player, hex_from_offset(1, 0)); // adjacent: ranged not needed
        let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone()], 1);
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.85;
        let _abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: focus.entity };

        let cast_focus = || PlanStep::Cast {
            ability: "melee_attack".into(),
            target: focus.entity,
            target_pos: focus.pos,
        };
        let build = |steps: Vec<PlanStep>| {
            let len = steps.len();
            TurnPlan {
                steps,
                final_pos: hex_from_offset(0, 0), // actor stays at start
                residual_ap: 0,
                residual_mp: 0,
                outcomes: vec![StepOutcome::default(); len],
                partial_score: 0.0,
                sim_snapshots: vec![snap.clone(); len],
            }
        };

        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        // Single-cast: per-step intent_score for melee_attack@focus.
        let single = compute_plan_factors(&build(vec![cast_focus()]), &intent, &scoring_ctx);
        let s1 = single.intent;
        assert!(s1 > 0.0, "single cast@focus must produce positive intent: {s1}");

        // Two casts (step-1c): first Cast per-step + terminal pursuit for tail.
        // cast_pos = actor.pos = (0,0), final_pos = (0,0), focus at (1,0).
        // dist(final, focus) = 1 <= reach=4 → pursuit returns 0.8.
        // intent = s1 + 0.8 × 0.85
        let reach = (actor.speed.max(0) as u32).saturating_add(actor.max_attack_range);
        let tail_pursuit = pursuit_move_score(actor.pos, hex_from_offset(0, 0), focus.pos, reach);
        let expected_two = s1 + tail_pursuit * 0.85;
        let two = compute_plan_factors(&build(vec![cast_focus(), cast_focus()]), &intent, &scoring_ctx);
        assert!(
            (two.intent - expected_two).abs() < 0.005,
            "two casts: intent={}, expected≈{expected_two} (s1={s1}, tail_pursuit={tail_pursuit})", two.intent,
        );

        // Three casts: same formula — tail still collapses to single pursuit.
        // Second and third Casts are both in the tail after first Cast.
        let expected_three = expected_two; // tail shortcut is the same regardless of tail length
        let three = compute_plan_factors(
            &build(vec![cast_focus(), cast_focus(), cast_focus()]),
            &intent, &scoring_ctx,
        );
        assert!(
            (three.intent - expected_three).abs() < 0.005,
            "three casts: intent={}, expected≈{expected_three} (same tail shortcut as two)", three.intent,
        );
    }

    /// Post-goal must not penalise further useful actions. Two identical
    /// two-Cast plans scored the same — one has step-0's cached `killed`
    /// listing the intent target (goal achieved), the other doesn't.
    /// Their `damage_sum` must match: step_weight stays pure geometric,
    /// without the old ×0.5 post-goal bump that used to halve subsequent
    /// step contributions.
    #[test]
    fn post_goal_leaves_step_weight_purely_geometric() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let target = unit(2, Team::Player, hex_from_offset(1, 0));
        let other = unit(3, Team::Player, hex_from_offset(2, 0));
        let snap = BattleSnapshot::new(
            vec![actor.clone(), target.clone(), other.clone()],
            1,
        );
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let _abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };

        let steps = vec![
            PlanStep::Cast {
                ability: "melee_attack".into(),
                target: target.entity,
                target_pos: target.pos,
            },
            PlanStep::Cast {
                ability: "melee_attack".into(),
                target: other.entity,
                target_pos: other.pos,
            },
        ];
        let mk = |outcomes: Vec<StepOutcome>| TurnPlan {
            steps: steps.clone(),
            final_pos: actor.pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes,
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };

        let goal_achieved = mk(vec![
            StepOutcome { killed: vec![target.entity], ..Default::default() },
            StepOutcome::default(),
        ]);
        let goal_missed = mk(vec![
            StepOutcome::default(),
            StepOutcome::default(),
        ]);

        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);
        let f_goal = compute_plan_factors(&goal_achieved, &intent, &scoring_ctx);
        let f_miss = compute_plan_factors(&goal_missed, &intent, &scoring_ctx);

        // step_weight stays purely geometric — every Cast-accumulating
        // factor should be equal between the two plans regardless of
        // whether step 0's outcome killed the intent target. Intent
        // itself does differ (post-goal skips it), not asserted here.
        for (got, want, name) in [
            (f_goal.damage, f_miss.damage, "damage"),
            (f_goal.kill_now, f_miss.kill_now, "kill_now"),
            (f_goal.kill_promised, f_miss.kill_promised, "kill_promised"),
            (f_goal.cc, f_miss.cc, "cc"),
            (f_goal.heal, f_miss.heal, "heal"),
            (f_goal.scarcity, f_miss.scarcity, "scarcity"),
        ] {
            assert_eq!(
                got, want,
                "{name}_sum must not depend on intent-kill status (step_weight stays geometric)",
            );
        }
    }

    /// `rescore_with_intent` must produce the same scores as a fresh
    /// `score_plans_with_raw` under the target intent. Pins the split:
    /// reusing intent-independent factor columns and re-filling only
    /// `factor[7]` cannot drift from the full recompute path.
    #[test]
    fn rescore_matches_full_score_under_same_intent() {
        use crate::combat::ai::planning::types::{PlanStep, StepOutcome};

        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let focus_a = unit(2, Team::Player, hex_from_offset(3, 0));
        let focus_b = unit(3, Team::Player, hex_from_offset(2, 0));
        let snap = BattleSnapshot::new(
            vec![actor.clone(), focus_a.clone(), focus_b.clone()],
            1,
        );
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        // Deterministic per-plan noise: rescore and a fresh-score under the
        // same intent produce identical scores regardless of profile.
        let difficulty = DifficultyProfile::epic();
        let _abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();

        let mk_plan = |target: &UnitSnapshot| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: target.entity,
                target_pos: target.pos,
            }],
            final_pos: actor.pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };
        let plans = vec![mk_plan(&focus_a), mk_plan(&focus_b)];

        let intent_a = TacticalIntent::FocusTarget { target: focus_a.entity };
        let intent_b = TacticalIntent::FocusTarget { target: focus_b.entity };

        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);
        let (_, mut raw) = score_plans_with_raw(&plans, &intent_a, &scoring_ctx);
        let rescored = rescore_with_intent(&plans, &mut raw, &intent_b, &scoring_ctx);
        let (full, _) = score_plans_with_raw(&plans, &intent_b, &scoring_ctx);

        // Noise is now deterministic per plan (not rng-driven), so rescore
        // and a fresh score under the same intent produce bitwise-equal
        // scores regardless of the `hard()` profile's zero-noise path.
        assert_eq!(
            rescored, full,
            "rescore under intent B must equal a fresh score under intent B",
        );
    }

    /// A deserialized `TurnPlan` arrives with empty `sim_snapshots` because of
    /// `#[serde(skip)]`. The scorer used to index `plan.sim_snapshots[idx - 1]`
    /// directly — any caller who fed it such a plan (e.g., a replay tool)
    /// would hit an OOB panic in release builds. `pre_step_snapshot` gracefully
    /// degrades to the initial `snap`, so factors go slightly stale but the
    /// pipeline stays crash-free.
    #[test]
    fn scorer_tolerates_empty_sim_snapshots_from_deserialized_plan() {
        use crate::combat::ai::planning::types::{PlanStep, StepOutcome};

        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0));
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let _abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: enemy.entity };

        // Multi-step plan with EMPTY sim_snapshots — matches the shape of a
        // plan round-tripped through serde.
        let deserialized_plan = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: enemy.entity,
                    target_pos: enemy.pos,
                },
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 0,
            residual_mp: 2,
            outcomes: vec![StepOutcome::default(), StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };

        // These must not panic despite `sim_snapshots` being empty. We don't
        // assert specific factor values — the fallback means multi-step
        // factors are computed against the initial snapshot, which is
        // intentionally stale. The guarantee is "safe, not accurate".
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);
        let factors = compute_plan_factors_sans_intent(&deserialized_plan, &scoring_ctx);
        let _ = factors;
        let intent_sum = compute_plan_intent_sum(&deserialized_plan, &intent, &scoring_ctx);
        let _ = intent_sum;

        let plans = vec![deserialized_plan];
        let (scores, raw) = score_plans_with_raw(&plans, &intent, &scoring_ctx);
        assert_eq!(scores.len(), 1);
        assert_eq!(raw.len(), 1);
        assert!(
            scores[0].is_finite(),
            "empty-sim_snapshots plan must still produce a finite score",
        );
    }

    /// Noise is seeded from `(round, actor, plan canonical key)`, so a given
    /// plan's score must stay the same regardless of where it sits in the
    /// plan pool. Pre-fix (rng.roll_d), the Nth plan drew the Nth roll, which
    /// meant reordering the pool (e.g. by `HashMap` iteration in
    /// `dedup_by_logical_key`) leaked a different noise vector under the same
    /// RNG seed. Pin the new invariant: scoring `[A, B]` vs `[B, A]` produces
    /// the same per-plan score.
    #[test]
    fn noise_is_plan_order_invariant() {
        use crate::combat::ai::planning::types::{PlanStep, StepOutcome};

        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let t_a = unit(2, Team::Player, hex_from_offset(3, 0));
        let t_b = unit(3, Team::Player, hex_from_offset(2, 0));
        let snap = BattleSnapshot::new(
            vec![actor.clone(), t_a.clone(), t_b.clone()],
            1,
        );
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        // Non-zero noise amplitude — only `easy()` has score_noise > 0 after the
        // noise-isolation refactor, so this pins that the invariant holds even when
        // noise actually contributes.
        let difficulty = DifficultyProfile::easy();
        assert!(
            difficulty.score_noise() > 0.0,
            "precondition: noise is non-zero under `easy` profile",
        );
        let _abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: t_a.entity };

        let mk_plan = |target: &UnitSnapshot| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: target.entity,
                target_pos: target.pos,
            }],
            final_pos: actor.pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };
        let plan_a = mk_plan(&t_a);
        let plan_b = mk_plan(&t_b);
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let (scores_ab, _) = score_plans_with_raw(
            &[plan_a.clone(), plan_b.clone()], &intent, &scoring_ctx,
        );
        let (scores_ba, _) = score_plans_with_raw(
            &[plan_b.clone(), plan_a.clone()], &intent, &scoring_ctx,
        );

        // scores_ab[0] is for plan_a; scores_ba[1] is also for plan_a.
        assert_eq!(
            scores_ab[0], scores_ba[1],
            "plan_a score must be position-independent",
        );
        assert_eq!(
            scores_ab[1], scores_ba[0],
            "plan_b score must be position-independent",
        );
    }

    /// `plan_trade_bonus` must reward killing a valuable target over a
    /// trivial one, all else equal. Both plans declare a kill in their
    /// `outcomes[0].killed`; the only difference is the victim's
    /// `unit_value` (driven here by `threat` through `horizon_avg`).
    /// Pins the architectural claim of MVP2 phase 3: the trade
    /// modifier actually differentiates the "what did we kill" signal
    /// that the binary `kill` factor can't.
    #[test]
    fn trade_bonus_favors_valuable_victim() {
        use crate::combat::ai::test_helpers::UnitBuilder;

        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let support = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .role(crate::combat::ai::role::AxisProfile { support: 1.0, ..Default::default() })
            .threat(6.0)
            .build();
        let rat = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0))
            .threat(1.0)
            .build();
        let snap = BattleSnapshot::new(
            vec![actor.clone(), support.clone(), rat.clone()],
            1,
        );
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let actor_val = unit_value(&actor, ctx.content);

        let mk_kill_plan = |victim: &UnitSnapshot| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: victim.entity,
                target_pos: victim.pos,
            }],
            final_pos: actor.pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome {
                killed: vec![victim.entity],
                ..Default::default()
            }],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };

        let b_support = plan_trade_bonus(
            &mk_kill_plan(&support), &actor, &snap, &ctx, actor_val,
        );
        let b_rat = plan_trade_bonus(
            &mk_kill_plan(&rat), &actor, &snap, &ctx, actor_val,
        );

        assert!(b_support > 0.0, "kill-support bonus must be positive: {b_support}");
        assert!(b_rat > 0.0, "kill-rat bonus still positive, just small: {b_rat}");
        assert!(
            b_support > b_rat,
            "trade_bonus must rank support-kill > rat-kill: {b_support} vs {b_rat}",
        );
    }

    /// End-to-end smoke: a self-lethal plan that kills a high-value
    /// support must out-score a passive plan, even under a non-
    /// ProtectSelf intent where MVP1 adaptation flips it to LastStand.
    /// This is the user-visible MVP2 behaviour — self-lethal-for-value
    /// is no longer strictly dominated by passive alternatives.
    ///
    /// Goes through `score_plans_with_raw` so the full pipeline
    /// (factors → normalisation → role weights → trade_bonus → noise)
    /// is exercised, not just the trade helper in isolation.
    #[test]
    fn self_lethal_kill_support_outscores_passive_under_last_stand() {
        use crate::combat::ai::test_helpers::UnitBuilder;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .ap(2)
            .threat(6.0)
            .ability_names(&["melee_attack"])
            .build();
        // High-value support: role=Support + strong threat drives
        // `unit_value` well above the actor's own.
        let support = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .role(crate::combat::ai::role::AxisProfile { support: 1.0, ..Default::default() })
            .threat(8.0)
            .build();
        // Provoker that guarantees AoO lethal on retreat from support.
        let provoker = UnitBuilder::new(3, Team::Player, hex_from_offset(0, 1))
            .aoo(5.0, 1)
            .build();
        let snap = BattleSnapshot::new(
            vec![actor.clone(), support.clone(), provoker.clone()],
            1,
        );
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        // Normal difficulty; score_noise > 0 — but the ranking assertion
        // below is robust to noise as long as the signal spread exceeds
        // the noise amplitude floor, which holds here (value of support
        // ≈ 2 × unit_value(self)).
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: support.entity };

        // Plan A — self-lethal cast killing support. `outcomes[0].killed`
        // encodes the kill that sim would observe; move-then-cast
        // triggers the AoO from `provoker` so `expected_aoo_damage ≥ hp`
        // flips the plan into the self-lethal trade branch.
        let plan_self_lethal = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: support.entity,
                    target_pos: support.pos,
                },
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![
                StepOutcome::default(),
                StepOutcome {
                    killed: vec![support.entity],
                    ..Default::default()
                },
            ],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };
        // Plan B — the "do nothing useful" baseline (EndTurn).
        let plan_passive = TurnPlan {
            steps: Vec::new(),
            final_pos: actor.pos,
            residual_ap: 2,
            residual_mp: 3,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let plans = vec![plan_passive, plan_self_lethal];
        let (scores, _) = score_plans_with_raw(&plans, &intent, &scoring_ctx);

        assert!(
            scores[1] > scores[0],
            "self-lethal kill-support ({}) must out-score passive ({})",
            scores[1], scores[0],
        );
    }

    // ── Step 1b: intent_sum for Move chains ─────────────────────────────────

    /// Regression pin for the "round-trip wins via intent_sum" bug:
    /// a pure-move plan is scored by its final position, not by path length.
    /// Three different path shapes to the same final tile must all produce
    /// the same intent_sum as a single-step plan ending there.
    #[test]
    fn pure_move_chain_intent_equals_single_pursuit() {
        // actor at (0,0), target far enough that reach doesn't flip the score.
        // speed=3, max_attack_range=1 → reach=4. Target at (6,0): dist=6 > 4.
        let actor = crate::combat::ai::test_helpers::UnitBuilder::new(
                1, Team::Enemy, hex_from_offset(0, 0))
            .speed(3)
            .max_attack_range(1)
            .build();
        let target = unit(2, Team::Player, hex_from_offset(6, 0));
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        // final_pos = (3, 0) for all plans. intermediate tiles differ.
        let final_pos = hex_from_offset(3, 0);
        let via_a = hex_from_offset(1, 0);
        let via_b = hex_from_offset(2, 0);

        let mk_move_plan = |steps: Vec<PlanStep>| {
            let n = steps.len();
            TurnPlan {
                steps,
                final_pos,
                residual_ap: 0,
                residual_mp: 0,
                outcomes: vec![StepOutcome::default(); n],
                partial_score: 0.0,
                sim_snapshots: (0..n).map(|_| snap.clone()).collect(),
            }
        };

        let one_step = mk_move_plan(vec![
            PlanStep::Move { path: vec![final_pos] },
        ]);
        let two_steps = mk_move_plan(vec![
            PlanStep::Move { path: vec![via_b] },
            PlanStep::Move { path: vec![final_pos] },
        ]);
        let three_steps = mk_move_plan(vec![
            PlanStep::Move { path: vec![via_a] },
            PlanStep::Move { path: vec![via_b] },
            PlanStep::Move { path: vec![final_pos] },
        ]);

        let s1 = compute_plan_intent_sum(&one_step, &intent, &scoring_ctx);
        let s2 = compute_plan_intent_sum(&two_steps, &intent, &scoring_ctx);
        let s3 = compute_plan_intent_sum(&three_steps, &intent, &scoring_ctx);

        assert!(s1 > 0.0, "single-step plan must have positive intent: {s1}");
        assert_eq!(s1, s2, "two-step plan to same final tile must equal one-step: s1={s1} s2={s2}");
        assert_eq!(s1, s3, "three-step plan to same final tile must equal one-step: s1={s1} s3={s3}");
    }

    /// Regression pin for the exact log case (line 12 of stormborn_camp log):
    /// round-trip plan [Move→A, Move→start, Move→C] must not outscore
    /// [Move→C] despite visiting more tiles. Both must equal the direct
    /// pursuit_move_score(start, C, target, reach).
    #[test]
    fn round_trip_pure_move_intent_no_credit() {
        let start = hex_from_offset(4, 4);
        let tile_a = hex_from_offset(4, 5);
        let tile_c = hex_from_offset(3, 6);
        let target_pos = hex_from_offset(1, 6); // arbitrary far target

        let actor = crate::combat::ai::test_helpers::UnitBuilder::new(
                1, Team::Enemy, start)
            .speed(3)
            .max_attack_range(1)
            .build();
        let target_unit = unit(2, Team::Player, target_pos);
        let snap = BattleSnapshot::new(vec![actor.clone(), target_unit.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target_unit.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        // Direct: one move to tile_c
        let direct = TurnPlan {
            steps: vec![PlanStep::Move { path: vec![tile_c] }],
            final_pos: tile_c,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };
        // Round-trip: A → start → C (same final)
        let round_trip = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![tile_a] },
                PlanStep::Move { path: vec![start] },
                PlanStep::Move { path: vec![tile_c] },
            ],
            final_pos: tile_c,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 3],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 3],
        };

        let s_direct = compute_plan_intent_sum(&direct, &intent, &scoring_ctx);
        let s_roundtrip = compute_plan_intent_sum(&round_trip, &intent, &scoring_ctx);

        assert_eq!(
            s_direct, s_roundtrip,
            "round-trip to same final tile must not outscore direct path: direct={s_direct} roundtrip={s_roundtrip}",
        );
    }

    /// Plans containing a Cast step must use per-step discounted accumulation,
    /// not the single-pursuit shortcut. The Cast step must contribute its full
    /// intent_score (via IntentWeights dot-product); Move steps before it also
    /// contribute their per-step pursuit score.
    #[test]
    fn cast_after_moves_keeps_cast_intent() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        // Actor needs weapon_dice so melee_attack produces non-zero damage factors.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        // Pure-cast plan: one Cast step. Measure its intent contribution as s_cast.
        let cast_step = PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };
        let pure_cast = TurnPlan {
            steps: vec![cast_step.clone()],
            final_pos: actor.pos,
            residual_ap: 1, residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };
        let s_cast_only = compute_plan_intent_sum(&pure_cast, &intent, &scoring_ctx);
        assert!(s_cast_only > 0.0, "cast-only plan must have positive intent: {s_cast_only}");

        // Move + Cast plan: Move to adjacent tile, then Cast.
        let move_then_cast = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                cast_step,
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 0, residual_mp: 2,
            outcomes: vec![StepOutcome::default(), StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };
        let s_move_cast = compute_plan_intent_sum(&move_then_cast, &intent, &scoring_ctx);

        // The Move+Cast plan is NOT pure-move, so it uses per-step accumulation.
        // It must yield a finite positive value that includes the cast's contribution.
        // We can't pin the exact value (Move pursuit score depends on geometry),
        // but we know s_cast_only is the cast's contribution at discount^1.
        // The Move+Cast result must be >= discount*s_cast_only (cast at step 1 with 0.9 weight).
        let min_expected = 0.9 * s_cast_only; // cast at discount^1
        assert!(
            s_move_cast >= min_expected,
            "Move+Cast intent_sum ({s_move_cast}) must include cast's discounted contribution (≥{min_expected})",
        );
        // Also must not equal the pure-move single-pursuit result (that would mean
        // the shortcut fired incorrectly on a plan with a Cast step).
        let reach = (actor.speed.max(0) as u32).saturating_add(actor.max_attack_range);
        let pursuit_only = pursuit_move_score(actor.pos, hex_from_offset(1, 0), target.pos, reach);
        assert_ne!(
            s_move_cast, pursuit_only,
            "Move+Cast must NOT use the pure-move shortcut (pursuit-only={pursuit_only})",
        );
    }

    /// Once the intent target is killed by a Cast step, the goal_achieved latch
    /// fires and subsequent Move steps must not contribute pursuit credit.
    /// Compares [Cast(kill), Move→A] with latch active vs. hypothetical without.
    #[test]
    fn goal_achieved_latch_still_works() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        // Plan: Cast(kill target) then Move away. The Move would give pursuit
        // credit if the latch didn't fire (target is dead, but pos still relevant).
        let plan_with_kill = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: target.entity,
                    target_pos: target.pos,
                },
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 1, residual_mp: 2,
            outcomes: vec![
                StepOutcome { killed: vec![target.entity], ..Default::default() },
                StepOutcome::default(),
            ],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };

        // Same plan but kill NOT recorded in outcomes — latch does not fire,
        // Move step's pursuit score accumulates.
        let plan_no_kill = TurnPlan {
            steps: plan_with_kill.steps.clone(),
            final_pos: plan_with_kill.final_pos,
            residual_ap: plan_with_kill.residual_ap,
            residual_mp: plan_with_kill.residual_mp,
            outcomes: vec![StepOutcome::default(), StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: plan_with_kill.sim_snapshots.clone(),
        };

        let s_with_kill = compute_plan_intent_sum(&plan_with_kill, &intent, &scoring_ctx);
        let s_no_kill = compute_plan_intent_sum(&plan_no_kill, &intent, &scoring_ctx);

        // With latch: only the Cast step contributes intent. Without latch: Cast +
        // discounted Move-pursuit also contributes. The latch plan must be ≤ no-latch
        // plan (Move pursuit could be positive or zero, but never negative for
        // approaching the target that we just killed — snap still shows it alive).
        assert!(
            s_with_kill <= s_no_kill,
            "goal_achieved latch must suppress post-kill Move pursuit credit: \
             with_kill={s_with_kill} no_kill={s_no_kill}",
        );
    }

    // ── Step 1c: post-Cast tail shortcut ────────────────────────────────────

    /// The post-first-Cast tail is collapsed into a single terminal pursuit,
    /// regardless of tail length. A plan [Move→A, Cast, Move→B, Move→C]
    /// must have the same intent_sum as [Move→A, Cast, Move→C] — the number
    /// of tail steps does not matter, only the final position does.
    #[test]
    fn cast_plus_move_tail_collapses_to_single_pursuit() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(3)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        // Target far away so dist > reach for the tail positions
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(8, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let tile_a = hex_from_offset(1, 0); // Move before Cast
        let cast_pos = tile_a;              // position at time of Cast
        let tile_b = hex_from_offset(2, 0);
        let tile_c = hex_from_offset(3, 0); // final destination for both plans

        let cast_step = PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };

        // Plan with long tail: Move→A, Cast, Move→B, Move→C
        let plan_long_tail = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![tile_a] },
                cast_step.clone(),
                PlanStep::Move { path: vec![tile_b] },
                PlanStep::Move { path: vec![tile_c] },
            ],
            final_pos: tile_c,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 4],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 4],
        };

        // Plan with short tail: Move→A, Cast, Move→C (same final)
        let plan_short_tail = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![tile_a] },
                cast_step,
                PlanStep::Move { path: vec![tile_c] },
            ],
            final_pos: tile_c,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 3],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 3],
        };

        let s_long = compute_plan_intent_sum(&plan_long_tail, &intent, &scoring_ctx);
        let s_short = compute_plan_intent_sum(&plan_short_tail, &intent, &scoring_ctx);

        // Terminal pursuit from cast_pos to tile_c: both plans land at the same
        // final_pos, so the tail contribution must be identical regardless of
        // how many Move steps the tail contains.
        assert!(
            (s_long - s_short).abs() < 0.001,
            "long-tail and short-tail plans with same final_pos must have equal intent: \
             long={s_long} short={s_short} cast_pos={cast_pos:?} final={tile_c:?}",
        );
    }

    /// Regression pin for the stormborn_camp line 23 bug:
    /// a Cast followed by a retreat tail (returning to cast_pos) must not earn
    /// post-Cast pursuit credit. `pursuit_move_score(cast_pos, cast_pos, ...)` = 0
    /// since there is no net displacement. The intent_sum equals just the Cast's
    /// per-step contribution, no tail bonus.
    #[test]
    fn cast_plus_roundtrip_tail_no_credit() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        let cast_pos = hex_from_offset(0, 6); // actor casts from here
        let actor = UnitBuilder::new(1, Team::Enemy, cast_pos)
            .ap(3)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(6, 6)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let cast_step = PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };

        // Cast-only plan: measures the Cast's per-step contribution
        let cast_only = TurnPlan {
            steps: vec![cast_step.clone()],
            final_pos: cast_pos,
            residual_ap: 0, residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };

        // Round-trip tail: Cast, then retreat back to cast_pos (net displacement = 0)
        let tile_retreat = hex_from_offset(0, 5);
        let round_trip = TurnPlan {
            steps: vec![
                cast_step.clone(),
                PlanStep::Move { path: vec![tile_retreat] },
                PlanStep::Move { path: vec![cast_pos] },
            ],
            final_pos: cast_pos, // same as cast_pos — zero net displacement
            residual_ap: 0, residual_mp: 1,
            outcomes: vec![StepOutcome::default(); 3],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 3],
        };

        let s_cast_only = compute_plan_intent_sum(&cast_only, &intent, &scoring_ctx);
        let s_round_trip = compute_plan_intent_sum(&round_trip, &intent, &scoring_ctx);

        // pursuit_move_score(cast_pos, cast_pos, target, reach) = 0: no displacement.
        // Round-trip tail earns zero post-Cast credit, equaling the cast-only plan.
        assert!(
            (s_round_trip - s_cast_only).abs() < 0.001,
            "round-trip tail must earn no post-Cast credit: \
             round_trip={s_round_trip} cast_only={s_cast_only}",
        );
    }

    /// Legitimate "cast then reposition toward target" earns positive post-Cast
    /// pursuit credit. A plan [Cast, Move→closer] must score higher than
    /// [Cast] alone when the final position brings the actor into range.
    #[test]
    fn cast_plus_approach_tail_earns_credit() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        // Actor far from target so even cast_pos is outside reach
        let cast_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, cast_pos)
            .ap(2)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        // Target at distance 8, reach = speed(3) + range(1) = 4
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(8, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let cast_step = PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };

        // Cast-only: no tail contribution
        let cast_only = TurnPlan {
            steps: vec![cast_step.clone()],
            final_pos: cast_pos,
            residual_ap: 0, residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone()],
        };

        // Cast then approach: move 3 tiles closer to target
        let closer_pos = hex_from_offset(3, 0); // dist to target = 5, still > reach=4
        let cast_then_approach = TurnPlan {
            steps: vec![
                cast_step,
                PlanStep::Move { path: vec![closer_pos] },
            ],
            final_pos: closer_pos,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 2],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 2],
        };

        let s_cast_only = compute_plan_intent_sum(&cast_only, &intent, &scoring_ctx);
        let s_approach = compute_plan_intent_sum(&cast_then_approach, &intent, &scoring_ctx);

        // Approach reduces distance (8→5 > reach), earning positive pursuit delta.
        // The "cast then reposition" pattern must be rewarded.
        assert!(
            s_approach > s_cast_only,
            "cast+approach to closer tile must score higher than cast-only: \
             approach={s_approach} cast_only={s_cast_only}",
        );
    }

    /// When the Cast kills the intent target, the goal_achieved latch fires
    /// and the post-Cast tail earns zero credit — pursuit is irrelevant when
    /// the goal is solved. Regression for the latch interaction with step-1c.
    #[test]
    fn cast_kills_then_tail_no_credit() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        let cast_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, cast_pos)
            .ap(2)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let cast_step = PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };

        // Cast that kills, then Move→closer_to_where_target_was
        let tile_a = hex_from_offset(3, 0);
        let plan_with_kill = TurnPlan {
            steps: vec![
                cast_step.clone(),
                PlanStep::Move { path: vec![tile_a] },
            ],
            final_pos: tile_a,
            residual_ap: 0, residual_mp: 2,
            outcomes: vec![
                StepOutcome { killed: vec![target.entity], ..Default::default() },
                StepOutcome::default(),
            ],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 2],
        };

        // Same plan, Cast does not kill — tail gets pursuit credit
        let plan_no_kill = TurnPlan {
            steps: plan_with_kill.steps.clone(),
            final_pos: tile_a,
            residual_ap: 0, residual_mp: 2,
            outcomes: vec![StepOutcome::default(); 2],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 2],
        };

        let s_with_kill = compute_plan_intent_sum(&plan_with_kill, &intent, &scoring_ctx);
        let s_no_kill = compute_plan_intent_sum(&plan_no_kill, &intent, &scoring_ctx);

        // Kill latches goal_achieved, tail contribution = 0.
        // No-kill plan gets tail pursuit credit (approaching a now-alive target).
        // Target at (1,0), tile_a at (3,0): dist(3,0 to 1,0)=2 > dist(0,0 to 1,0)=1
        // but dist(1,0 to 1,0)=0 <= reach, so pursuit_move_score = 0.8.
        // With kill: intent = cast_intent + 0 (latched).
        // Without kill: intent = cast_intent + 0.8 × 0.9.
        assert!(
            s_with_kill < s_no_kill,
            "goal_achieved latch must suppress post-Cast tail when Cast kills target: \
             with_kill={s_with_kill} no_kill={s_no_kill}",
        );
    }

    /// When a plan has two Casts [Cast(X), Cast(Y), Move→B], only the first
    /// Cast contributes per-step intent. Everything after (second Cast + Move)
    /// is collapsed into the terminal pursuit from first-cast position to
    /// final_pos. The second Cast must NOT receive its own per-step intent credit.
    #[test]
    fn cast_then_cast_then_move_uses_first_cast_as_boundary() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::content::abilities::CasterContext;
        use crate::core::DiceExpr;

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(3)
            .speed(3)
            .max_attack_range(1)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .caster_ctx(CasterContext {
                str_mod: 2,
                weapon_dice: Some(DiceExpr::new(1, 8, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(8, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_step_discount = 0.9;
        let ctx = test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let scoring_ctx = make_scoring_ctx(&ctx, &snap, &maps, &reservations, &actor);

        let cast_step = || PlanStep::Cast {
            ability: "melee_attack".into(),
            target: target.entity,
            target_pos: target.pos,
        };

        let final_pos = hex_from_offset(3, 0);

        // Two-cast + move plan: [Cast, Cast, Move→final]
        let plan_two_cast = TurnPlan {
            steps: vec![cast_step(), cast_step(), PlanStep::Move { path: vec![final_pos] }],
            final_pos,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 3],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 3],
        };

        // One-cast + move plan: [Cast, Move→final] — for comparison
        let plan_one_cast = TurnPlan {
            steps: vec![cast_step(), PlanStep::Move { path: vec![final_pos] }],
            final_pos,
            residual_ap: 0, residual_mp: 0,
            outcomes: vec![StepOutcome::default(); 2],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(); 2],
        };

        let s_two_cast = compute_plan_intent_sum(&plan_two_cast, &intent, &scoring_ctx);
        let s_one_cast = compute_plan_intent_sum(&plan_one_cast, &intent, &scoring_ctx);

        // Both plans: first Cast gets per-step credit, then terminal pursuit
        // from actor_pos to final_pos. The second Cast in plan_two_cast is
        // collapsed into the tail — it must NOT add extra per-step credit.
        // Therefore intent_sum must be equal for both plans.
        assert!(
            (s_two_cast - s_one_cast).abs() < 0.001,
            "second Cast in tail must not add extra per-step credit: \
             two_cast={s_two_cast} one_cast={s_one_cast}",
        );
    }

    /// Trade bonus on an inert plan (no kills, no self-lethal exposure)
    /// is exactly zero — the modifier must not drift the scoring of
    /// neutral plans. Baseline contrast against `_favors_valuable_victim`.
    #[test]
    fn trade_bonus_zero_for_neutral_plan() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::hard();
        let ctx = test_ctx(&content, &difficulty);
        let actor_val = unit_value(&actor, ctx.content);

        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: actor.entity,
                target_pos: actor.pos,
            }],
            final_pos: actor.pos,
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };

        let b = plan_trade_bonus(&plan, &actor, &snap, &ctx, actor_val);
        assert_eq!(b, 0.0);
    }
}
