//! PickBestStage — picks the winning plan, plus the picker API formerly in
//! `planning/picker.rs` (`PickMechanics`, `commit_plan`, `pick_best_plan`,
//! `record_committed_reservations`).
//!
//! Selection uses the same mercy + top-K window logic as `pick_best_plan`, then
//! writes `annotation.chosen` / `annotation.pick` on the winner.
//!
//! ## Per-agenda-item composition
//!
//! When `ctx.agenda` is `Some` and `ItemScoringStage` populated `per_item`, the
//! stage swaps the primary intent+tempo columns for each item's variant and
//! takes the best — the delta lives in the same additive space as
//! `finalize_scores`:
//!
//! ```text
//! composed_i = ann.score_initial
//!            + intent_contribution(per_item[i]) - intent_contribution(primary)
//!            + tempo_contribution(per_item[i])  - tempo_contribution(primary)
//!            + W × cdot_i                       // W = weight[PlanFactor::Intent]
//! ann.score = max_i(composed_i);  ann.agenda_item = argmax_i  // over eligible i
//! ```
//!
//! Scaling `cdot` by `W` caps the bonus at one factor-swing so it can't override
//! Sanity/Critics multipliers and adds no new tuning surface. The asymmetry
//! (fallback plans with no eligible item keep the raw pipeline score) is
//! intentional: having a band-eligible item is itself a quality signal.
//!
//! Edge cases: empty agenda / `per_item` → legacy single-score path; all items
//! `!eligible` → `ann.score` unchanged, `ann.agenda_item = None`.

use crate::combat::ai::orchestration::{AiDecision, AiWorld, MoveOrigin};
use crate::combat::ai::outcome::PickInfo;
use crate::combat::ai::pipeline::effects::SelectionKey;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::plan::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::scoring::factors::aggregate::factor_contribution;
use crate::combat::ai::scoring::factors::{
    aoe_area, aoe_hits, BatchStats, PlanFactor, PlanFactorValues, StepFactor,
};
use crate::combat::ai::world::reservations::Reservations;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use combat_engine::DiceRng;
use std::hash::{Hash, Hasher};

// ── Picker API (consolidated from planning/picker.rs) ─────────────────────────

/// Raw mechanics output from `pick_best_plan`. The outer layer converts pool
/// indices into human-readable labels for debug output.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct PickMechanics {
    pub top_k: usize,
    pub window: f32,
    pub mercy_margin: f32,
    pub mercy_applied: bool,
    /// `(plan_index, final_score)` in pool order.
    pub pool: Vec<(usize, f32)>,
    pub chosen_pos: usize,
}

/// Commit the winning plan's first step (or first two, if they're a
/// Move→Cast bundle) as a single `AiDecision`, along with how many steps
/// of the plan the decision consumed. The remainder of the plan is
/// discarded — every AI tick re-plans from scratch.
///
/// Bundling rules (`consumed` follows the match arm):
/// - Empty plan → `EndTurn`, 0 steps.
/// - `[Cast, ..]` → `CastInPlace`, 1 step.
/// - `[Move, Cast, ..]` → `MoveAndCast` (or `CastInPlace` if the move path
///   is empty), 2 steps. One atomic tick preserves the engine contract
///   (one `UseAbility` per actor-turn pathfind).
/// - `[Move, ..]` → `Move { origin: BestPlan }` (or `EndTurn` when the path is
///   a no-op), 1 step.
pub fn commit_plan(plan: &TurnPlan, actor_pos: Hex) -> (AiDecision, usize) {
    let prefix = plan.committed_prefix();
    let consumed = prefix.step_count();
    let decision = match prefix {
        CommittedPrefix::EndTurn => AiDecision::EndTurn,
        CommittedPrefix::Cast {
            ability,
            target,
            target_pos,
        } => AiDecision::CastInPlace {
            ability: ability.clone(),
            target,
            target_pos,
        },
        CommittedPrefix::MoveThenCast {
            path,
            ability,
            target,
            target_pos,
        } => {
            // Degenerate bundle (empty move path) collapses to a bare cast.
            if path.is_empty() {
                AiDecision::CastInPlace {
                    ability: ability.clone(),
                    target,
                    target_pos,
                }
            } else {
                AiDecision::MoveAndCast {
                    path: path.to_vec(),
                    ability: ability.clone(),
                    target,
                    target_pos,
                }
            }
        }
        CommittedPrefix::MoveOnly { path } => {
            // Degenerate move (empty path or stays put) ends the turn instead.
            let dest = path.last().copied().unwrap_or(actor_pos);
            if path.is_empty() || dest == actor_pos {
                AiDecision::EndTurn
            } else {
                AiDecision::Move {
                    path: path.to_vec(),
                    origin: MoveOrigin::BestPlan,
                }
            }
        }
    };
    (decision, consumed)
}

/// Mercy cruelty for a plan: how harsh does it feel? Kill dominates; CC caps
/// at 0.5 regardless of magnitude. Reads the precomputed raw factor row.
fn mercy_cruelty(raw: &PlanFactorValues) -> f32 {
    raw.get(StepFactor::KillNow)
        + raw.get(StepFactor::KillPromised) * 0.5
        + (raw.get(StepFactor::Cc) * 0.1).min(0.5)
}

/// Pick the winning plan: window-bounded top-K sampling with a mercy tie-breaker
/// applied only inside the near-best window.
///
/// Always returns the `PickMechanics` breakdown — prod callers ignore it, the
/// debug overlay reads it. The pool is ≤ `top_k` (1-3 in practice), too cheap to
/// justify a dual streaming-vs-materialize path.
pub fn pick_best_plan(
    keys: &[SelectionKey],
    raw_factors: &[PlanFactorValues],
    ctx: &AiWorld,
    rng: &mut DiceRng,
) -> (usize, PickMechanics) {
    let top_k_req = ctx.difficulty.top_k_choice();
    let m = ctx.difficulty.mercy_margin();
    let window = (ctx.difficulty.score_noise() * 2.0).max(0.05);

    let make_mech = |top_k, mercy_applied, pool, chosen_pos| PickMechanics {
        top_k,
        window,
        mercy_margin: m,
        mercy_applied,
        pool,
        chosen_pos,
    };

    if keys.is_empty() {
        return (0, make_mech(top_k_req, false, vec![], 0));
    }

    // Sort by SelectionKey desc — selectable plans first, then by score within bucket.
    let mut ranked: Vec<(usize, SelectionKey)> = keys.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    let best = ranked[0].1;
    let best_score = best.score;
    let mut mercy_applied = false;
    // Mercy only triggers if best is selectable (preserves old `is_finite()` semantics).
    if m > 0.0 && best.selectable {
        let mercy_end = ranked
            .iter()
            .position(|(_, k)| !k.selectable || k.score < best_score - m)
            .unwrap_or(ranked.len());
        if mercy_end > 1 {
            let mut windowed: Vec<(usize, f32)> = ranked[..mercy_end]
                .iter()
                .map(|&(i, k)| (i, k.score - m * mercy_cruelty(&raw_factors[i])))
                .collect();
            windowed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            // Update ranked: keep SelectionKey selectable=true (mercy doesn't change bucket),
            // only the score field changes.
            for (slot, (idx, new_score)) in windowed.into_iter().enumerate() {
                ranked[slot] = (
                    idx,
                    SelectionKey {
                        selectable: true,
                        score: new_score,
                    },
                );
            }
            mercy_applied = true;
        }
    }

    let k_top = top_k_req.max(1).min(ranked.len());
    let best_after = ranked[0].1.score;

    let pool: Vec<(usize, f32)> = ranked
        .iter()
        .take(k_top)
        .filter(|(_, k)| k.selectable && k.score >= best_after - window)
        .map(|&(i, k)| (i, k.score))
        .collect();

    if pool.is_empty() {
        return (
            ranked[0].0,
            make_mech(
                k_top,
                mercy_applied,
                vec![(ranked[0].0, ranked[0].1.score)],
                0,
            ),
        );
    }
    // `roll_d(N)` returns `1..=N`; shift to a 0-based index.
    let chosen_pos = (rng.roll_d(pool.len() as u32) - 1) as usize;
    let chosen_idx = pool[chosen_pos].0;
    (
        chosen_idx,
        make_mech(k_top, mercy_applied, pool, chosen_pos),
    )
}

/// Record reservations for the **committed** prefix (first `consumed` steps,
/// the ones this tick emits as an `AiDecision`) so subsequent AI units this
/// round coordinate (avoid overkill, duplicate CC, tile collisions). Later plan
/// steps stay invisible until they commit on a future tick — trades a weaker
/// signal for no ghost reservations when plans are invalidated mid-flight.
pub fn record_committed_reservations(
    plan: &TurnPlan,
    consumed: usize,
    active: UnitView<'_>,
    ctx: &AiWorld,
    snap: &BattleSnapshot,
    reservations: &mut Reservations,
    actor_pos: Hex,
) {
    let mut resting_tile = actor_pos;
    for (idx, step, caster_tile) in plan.walk_with_caster(actor_pos).take(consumed) {
        // After a Move, `walk_with_caster` advances to the destination on
        // the *next* yield — track the post-step caster ourselves so the
        // final reservation uses the resting tile after the committed prefix.
        if let PlanStep::Move { path } = step {
            if let Some(&dest) = path.last() {
                resting_tile = dest;
            }
        }
        let PlanStep::Cast {
            ability,
            target,
            target_pos,
        } = step
        else {
            continue;
        };
        let Some(def) = ctx.content.abilities.get(ability) else {
            continue;
        };
        let is_cc = applies_cc(def, ctx.content);
        let hits: Vec<Entity> = if def.aoe == AoEShape::None {
            vec![*target]
        } else {
            let area = aoe_area(def, *target_pos, caster_tile);
            aoe_hits(&area, active, snap)
                .enemies
                .iter()
                .map(|e| e.entity())
                .collect()
        };
        for ent in hits {
            if let Some(_target_unit) = snap.unit(ent) {
                if def.target_type != TargetType::SingleAlly {
                    // Raw damage fact for reservation bookkeeping.
                    // For AoE: use per-entity breakdown if available (avoids
                    // over-reserving aggregated total per target). For single-target:
                    // use enemy_damage directly.
                    let dmg = plan.annotation.outcomes.get(idx).map_or(0.0, |o| {
                        if o.enemy_damage_per_entity.is_empty() {
                            o.enemy_damage
                        } else {
                            o.enemy_damage_per_entity
                                .iter()
                                .find(|(e, _)| *e == ent)
                                .map_or(0.0, |(_, d)| *d)
                        }
                    });
                    if dmg > 0.0 {
                        reservations.reserve_damage(ent, dmg);
                    }
                }
                if is_cc {
                    reservations.reserve_cc(ent);
                }
            }
        }
    }

    // Reserve the tile we'll actually stop on this tick (end of the committed
    // prefix), not the plan's eventual `final_pos` — same no-ghost principle.
    if resting_tile != actor_pos {
        reservations.reserve_tile(resting_tile);
    }
}

// ── PickBestStage ─────────────────────────────────────────────────────────────

pub struct PickBestStage;

impl PlanStage for PickBestStage {
    fn name(&self) -> &'static str {
        "pick_best"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        if pool.is_empty() {
            return;
        }

        if let Some(agenda) = ctx.agenda {
            if !agenda.items.is_empty() {
                let has_per_item = pool.annotations.iter().any(|a| !a.per_item.is_empty());
                if has_per_item {
                    let band_weights = agenda.band.weights();
                    let scoring = ctx.scoring;

                    // Reconstruct BatchStats for Intent + TempoGain only (the
                    // two plan factors the additive deltas need). O(N) over pool.
                    let mut stats_intent = BatchStats { min: 0.0, max: 0.0 };
                    let mut stats_tempo = BatchStats { min: 0.0, max: 0.0 };

                    for ann in pool.annotations.iter() {
                        let vi = ann.factors.get_plan(PlanFactor::Intent);
                        if vi > stats_intent.max {
                            stats_intent.max = vi;
                        }
                        if vi < stats_intent.min {
                            stats_intent.min = vi;
                        }

                        let vt = ann.factors.get_plan(PlanFactor::TempoGain);
                        if vt > stats_tempo.max {
                            stats_tempo.max = vt;
                        }
                        if vt < stats_tempo.min {
                            stats_tempo.min = vt;
                        }
                    }

                    // Also fold in per_item values so the batch range covers
                    // the full domain that composition will see.
                    for ann in pool.annotations.iter() {
                        for pi in ann.per_item.iter() {
                            if pi.intent_factor > stats_intent.max {
                                stats_intent.max = pi.intent_factor;
                            }
                            if pi.intent_factor < stats_intent.min {
                                stats_intent.min = pi.intent_factor;
                            }
                            if pi.tempo_factor > stats_tempo.max {
                                stats_tempo.max = pi.tempo_factor;
                            }
                            if pi.tempo_factor < stats_tempo.min {
                                stats_tempo.min = pi.tempo_factor;
                            }
                        }
                    }

                    // Reconstruct Intent/TempoGain weights — must match the
                    // weighting in aggregate_factors_to_score.
                    let world = scoring.world;
                    let active = scoring.active;
                    let mut weights = if scoring.last_goal.is_some() {
                        active.cache.role.factor_weights_continuation(world.tuning)
                    } else {
                        active.cache.role.factor_weights(world.tuning)
                    };
                    weights[StepFactor::count() + PlanFactor::Intent as usize] *=
                        world.difficulty.intent_commitment;
                    // Scarcity modulation doesn't affect Intent/TempoGain slots;
                    // included here only for completeness / future-proof.
                    weights[StepFactor::Scarcity as usize] *= world.difficulty.resource_discipline;

                    let w_intent = weights[StepFactor::count() + PlanFactor::Intent as usize];
                    let w_tempo = weights[StepFactor::count() + PlanFactor::TempoGain as usize];

                    for ann in pool.annotations.iter_mut() {
                        if ann.per_item.is_empty() {
                            continue;
                        }

                        // Primary intent/tempo values from the finalized factor columns.
                        let intent_primary = ann.factors.get_plan(PlanFactor::Intent);
                        let tempo_primary = ann.factors.get_plan(PlanFactor::TempoGain);

                        let contrib_intent_primary = factor_contribution(
                            intent_primary,
                            &stats_intent,
                            PlanFactor::Intent.signed(),
                            w_intent,
                        );
                        let contrib_tempo_primary = factor_contribution(
                            tempo_primary,
                            &stats_tempo,
                            PlanFactor::TempoGain.signed(),
                            w_tempo,
                        );

                        let mut best_composed: Option<(f32, u8)> = None;

                        for (item_idx, (_item, per_item)) in
                            agenda.items.iter().zip(ann.per_item.iter()).enumerate()
                        {
                            // Skip ineligible items (ProtectSelf / FocusTarget masking).
                            if !per_item.eligible {
                                continue;
                            }

                            // Additive intent delta: swap primary column with per-item column.
                            let contrib_intent_item = factor_contribution(
                                per_item.intent_factor,
                                &stats_intent,
                                PlanFactor::Intent.signed(),
                                w_intent,
                            );
                            let intent_delta = contrib_intent_item - contrib_intent_primary;

                            // Additive tempo delta: same pattern.
                            let contrib_tempo_item = factor_contribution(
                                per_item.tempo_factor,
                                &stats_tempo,
                                PlanFactor::TempoGain.signed(),
                                w_tempo,
                            );
                            let tempo_delta = contrib_tempo_item - contrib_tempo_primary;

                            // per_item.considerations is the composite of item-level
                            // (urgency/role_affinity, plan-agnostic) and overlay
                            // (feasibility/leverage/safety/continuation_value, plan-aware)
                            // signals. PickBest reads the composite only.
                            let cdot = per_item.considerations.weighted_dot(&band_weights);

                            // composed = initial + intent_delta + tempo_delta + W × cdot
                            let composed =
                                ann.score_initial + intent_delta + tempo_delta + w_intent * cdot;

                            if best_composed.is_none_or(|(best, _)| composed > best) {
                                best_composed = Some((composed, item_idx as u8));
                            }
                        }

                        // Populated unconditionally so the log contains the full
                        // overlay even for non-chosen / ineligible items.
                        ann.considerations_per_item =
                            ann.per_item.iter().map(|pi| pi.considerations).collect();

                        // Step 11.7: snapshot reject reasons alongside considerations.
                        ann.reject_reasons_per_item =
                            ann.per_item.iter().map(|pi| pi.reject_reason).collect();

                        if let Some((best_score, best_idx)) = best_composed {
                            ann.set_score(best_score);
                            ann.agenda_item = Some(best_idx);
                        }
                        // If no eligible item: ann.score stays (pipeline value),
                        // ann.agenda_item stays None. Fallback path — see module doc.
                    }
                }
            }
        }

        // ── Jitter + argmax (shared path) ─────────────────────────────────────
        let noise_per_plan = apply_pick_jitter(pool, ctx);

        let keys: Vec<SelectionKey> = pool.annotations.iter().map(|a| a.selection_key()).collect();
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();

        let (best_idx, mech) = pick_best_plan(&keys, &raw_factors, ctx.scoring.world, ctx.rng);

        pool.annotations[best_idx].chosen = true;
        pool.annotations[best_idx].pick = Some(PickInfo {
            mechanics: mech,
            noise_applied: noise_per_plan[best_idx],
        });
    }
}

// ── Picking jitter ────────────────────────────────────────────────────────────

/// Apply deterministic, batch-scaled noise to every selectable plan before
/// argmax, mutating `ann.score` in place. Returns per-plan noise (0.0 for
/// skipped / masked plans). All plans masked (non-finite spread) → zero vec, no
/// constant-spread fallback.
fn apply_pick_jitter(pool: &mut ScoredPool, ctx: &StageCtx) -> Vec<f32> {
    let noise_amp = ctx.scoring.world.difficulty.score_noise();
    let n = pool.len();
    let mut noise_per_plan = vec![0.0_f32; n];

    if noise_amp <= 0.0 || n == 0 {
        return noise_per_plan;
    }

    let (s_min, s_max) = pool
        .annotations
        .iter()
        .filter(|ann| ann.is_selectable())
        .map(|ann| ann.score)
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s), hi.max(s))
        });

    // Spec semantics: early return if no selectable scores (all masked).
    if !s_min.is_finite() || !s_max.is_finite() {
        return noise_per_plan;
    }

    let spread = (s_max - s_min).max(0.05);
    let effective_amp = noise_amp * spread;

    let actor = ctx.scoring.active.entity();
    let round = ctx.scoring.snap.state.round;

    for (i, (plan, ann)) in pool
        .plans
        .iter()
        .zip(pool.annotations.iter_mut())
        .enumerate()
    {
        if !ann.is_selectable() {
            continue;
        }
        let n = plan_noise_internal(plan, round, actor, effective_amp);
        ann.set_score(ann.score + n);
        noise_per_plan[i] = n;
    }

    noise_per_plan
}

/// Deterministic per-plan noise ∈ [−amp, +amp). Seed = hash((round, actor,
/// plan canonical key)) — order-invariant across any permutation of the plan
/// pool.
fn plan_noise_internal(plan: &TurnPlan, round: u32, actor: Entity, amp: f32) -> f32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    round.hash(&mut h);
    actor.hash(&mut h);
    plan.hash_canonical(plan_start_tile(plan), &mut h);
    let bits = h.finish();
    let u = ((bits >> 40) as u32) as f32 / (1u32 << 24) as f32;
    (u * 2.0 - 1.0) * amp
}

/// Returns a stable start tile for plan canonical hashing.
fn plan_start_tile(plan: &TurnPlan) -> Hex {
    plan.final_pos
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "pick_best_tests.rs"]
mod tests;
