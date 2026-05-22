//! PickBestStage — step 7.4 / 11.4.
//!
//! Also contains the picker API (formerly `planning/picker.rs`):
//! `PickMechanics`, `commit_plan`, `pick_best_plan`, `record_committed_reservations`.
//!
//! Selects the winning plan from the scored pool using the same mercy + top-K
//! window logic that `PlanRanking::pick` used (via `pick_best_plan`). Writes
//! `annotation.chosen = true` and `annotation.pick = Some(PickInfo { .. })`
//! on the winning plan.
//!
//! ## Per-agenda-item composition (step 11.4)
//!
//! When `ctx.agenda` is `Some` and plans have `per_item` data populated by
//! `ItemScoringStage`, the stage performs **additive composition**:
//!
//! ```text
//! For each eligible per_item[i]:
//!   composed_i = ann.score_initial
//!              + factor_contribution(per_item.intent_factor, stats_intent, signed, w_intent)
//!              - factor_contribution(intent_primary,          stats_intent, signed, w_intent)
//!              + factor_contribution(per_item.tempo_factor,  stats_tempo,  signed, w_tempo)
//!              - factor_contribution(tempo_primary,           stats_tempo,  signed, w_tempo)
//!              + w_intent × cdot_i
//!
//! ann.score       = max_i(composed_i)  over eligible items
//! ann.agenda_item = argmax_i
//! ```
//!
//! The formula replaces the primary-intent and tempo columns with per-item
//! variants, computing the *delta* in the same additive space as `finalize_scores`.
//! The `cdot` bonus uses `w_intent` as a scale cap so it cannot override
//! Sanity / Critics multipliers.
//!
//! # Asymmetry: attributed vs fallback plans
//!
//! Per-item composition (step 11.4):
//!   attributed plan: composed = initial + intent_delta + tempo_delta + W × cdot
//!   fallback plan:   composed = pipeline ann.score (no eligible items)
//!
//! W = weight[PlanFactor::Intent] from finalize_scores — keeps cdot bonus
//! in scale of one factor swing, no new tuning surface.
//!
//! Asymmetry is intentional: having a band-eligible item IS a quality signal.
//! Bounded by W so it cannot override Sanity/Critics multipliers.
//!
//! **Edge cases:**
//! - Empty agenda or `per_item` empty → legacy single-score path.
//! - All items `!eligible` → `ann.score` stays as-is (pipeline value), `ann.agenda_item = None`.

use crate::combat::ai::scoring::factors::{aoe_area, aoe_hits, BatchStats, PlanFactor, PlanFactorValues, StepFactor};
use crate::combat::ai::outcome::PickInfo;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::pipeline::effects::SelectionKey;
use crate::combat::ai::scoring::factors::aggregate::factor_contribution;
use crate::combat::ai::plan::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::orchestration::{AiDecision, AiWorld, MoveOrigin};
use crate::combat::ai::world::reservations::Reservations;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::DiceRng;
use crate::game::hex::Hex;
use bevy::prelude::Entity;
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
    // Structural decomposition lives on TurnPlan; we only decide how each
    // prefix shape maps to an `AiDecision` (with a few no-op short-circuits
    // for empty-path degenerate cases).
    let prefix = plan.committed_prefix();
    let consumed = prefix.step_count();
    let decision = match prefix {
        CommittedPrefix::EndTurn => AiDecision::EndTurn,
        CommittedPrefix::Cast { ability, target, target_pos } => AiDecision::CastInPlace {
            ability: ability.clone(),
            target,
            target_pos,
        },
        CommittedPrefix::MoveThenCast { path, ability, target, target_pos } => {
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
/// at 0.5 regardless of magnitude. Reads the **precomputed** raw factor row
/// for `plan` — previously we re-ran `compute_plan_factors` per plan in the
/// mercy window, which was a full plan-walk + per-step factor recomputation
/// just to grab two numbers we already had.
fn mercy_cruelty(raw: &PlanFactorValues) -> f32 {
    raw.get(StepFactor::KillNow)
        + raw.get(StepFactor::KillPromised) * 0.5
        + (raw.get(StepFactor::Cc) * 0.1).min(0.5)
}

/// Pick the winning plan. Mirrors `pick_best_candidate` — window-bounded top-K
/// sampling with a mercy tie-breaker applied only inside the near-best window.
///
/// Always returns the `PickMechanics` breakdown (top_k, window, mercy
/// bookkeeping, ranked pool). The pool is ≤ `top_k` elements (1-3 in practice),
/// so the allocation is ~24 bytes on the stack / small-Vec region — too cheap
/// to justify a dual streaming-vs-materialize path. Prod callers ignore the
/// mechanics; debug overlay reads it.
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
                ranked[slot] = (idx, SelectionKey { selectable: true, score: new_score });
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
            make_mech(k_top, mercy_applied, vec![(ranked[0].0, ranked[0].1.score)], 0),
        );
    }
    // `roll_d(N)` returns `1..=N`; shift to a 0-based index.
    let chosen_pos = (rng.roll_d(pool.len() as u32) - 1) as usize;
    let chosen_idx = pool[chosen_pos].0;
    (chosen_idx, make_mech(k_top, mercy_applied, pool, chosen_pos))
}

/// Record reservations for the **committed** prefix of the winning plan so
/// subsequent AI units this round coordinate (avoid overkill, duplicate CC,
/// tile collisions). Only the first `consumed` steps — the ones this tick
/// actually emits as an `AiDecision` — are recorded. Future plan steps stay
/// invisible to the reservation layer until they themselves commit on a later
/// tick; this trades a slightly weaker coordination signal for freedom from
/// ghost reservations when plans get invalidated mid-flight.
///
/// `consumed` comes from `steps_consumed_by_decision` and matches the match
/// arm in `decision_from_steps` (1 for a solo cast/move, 2 for a Move→Cast
/// bundle).
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
        let PlanStep::Cast { ability, target, target_pos } = step else { continue };
        let Some(def) = ctx.content.abilities.get(ability) else { continue };
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

        // ── Step 11.4: per-agenda-item additive composition ───────────────────
        //
        // When an agenda is present and ItemScoringStage has filled `per_item`,
        // rewrite ann.score to the composed per-item maximum before jitter.
        if let Some(agenda) = ctx.agenda {
            if !agenda.items.is_empty() {
                let has_per_item = pool.annotations.iter().any(|a| !a.per_item.is_empty());
                if has_per_item {
                    let band_weights = agenda.band.weights();
                    let scoring = ctx.scoring;

                    // ── Reconstruct BatchStats for Intent and TempoGain (O(N)) ──
                    // This mirrors finalize_scores Pass 0 but only for the two
                    // plan factors needed for additive deltas. O(N) over pool.
                    let mut stats_intent = BatchStats { min: 0.0, max: 0.0 };
                    let mut stats_tempo  = BatchStats { min: 0.0, max: 0.0 };

                    for ann in pool.annotations.iter() {
                        let vi = ann.factors.get_plan(PlanFactor::Intent);
                        if vi > stats_intent.max { stats_intent.max = vi; }
                        if vi < stats_intent.min { stats_intent.min = vi; }

                        let vt = ann.factors.get_plan(PlanFactor::TempoGain);
                        if vt > stats_tempo.max { stats_tempo.max = vt; }
                        if vt < stats_tempo.min { stats_tempo.min = vt; }
                    }

                    // Also fold in per_item values so the batch range covers
                    // the full domain that composition will see.
                    for ann in pool.annotations.iter() {
                        for pi in ann.per_item.iter() {
                            if pi.intent_factor > stats_intent.max { stats_intent.max = pi.intent_factor; }
                            if pi.intent_factor < stats_intent.min { stats_intent.min = pi.intent_factor; }
                            if pi.tempo_factor > stats_tempo.max  { stats_tempo.max = pi.tempo_factor; }
                            if pi.tempo_factor < stats_tempo.min  { stats_tempo.min = pi.tempo_factor; }
                        }
                    }

                    // ── Reconstruct weights for Intent and TempoGain ──
                    // Mirrors finalize_scores lines 183-201 exactly.
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
                    weights[StepFactor::Scarcity as usize] *=
                        world.difficulty.resource_discipline;

                    let w_intent = weights[StepFactor::count() + PlanFactor::Intent as usize];
                    let w_tempo  = weights[StepFactor::count() + PlanFactor::TempoGain as usize];

                    for ann in pool.annotations.iter_mut() {
                        if ann.per_item.is_empty() {
                            continue;
                        }

                        // Primary intent/tempo values from the finalized factor columns.
                        let intent_primary = ann.factors.get_plan(PlanFactor::Intent);
                        let tempo_primary  = ann.factors.get_plan(PlanFactor::TempoGain);

                        let contrib_intent_primary =
                            factor_contribution(intent_primary, &stats_intent, PlanFactor::Intent.signed(), w_intent);
                        let contrib_tempo_primary =
                            factor_contribution(tempo_primary, &stats_tempo, PlanFactor::TempoGain.signed(), w_tempo);

                        let mut best_composed: Option<(f32, u8)> = None;

                        for (item_idx, (_item, per_item)) in agenda
                            .items
                            .iter()
                            .zip(ann.per_item.iter())
                            .enumerate()
                        {
                            // Skip ineligible items (ProtectSelf / FocusTarget masking).
                            if !per_item.eligible {
                                continue;
                            }

                            // Additive intent delta: swap primary column with per-item column.
                            let contrib_intent_item =
                                factor_contribution(per_item.intent_factor, &stats_intent, PlanFactor::Intent.signed(), w_intent);
                            let intent_delta = contrib_intent_item - contrib_intent_primary;

                            // Additive tempo delta: same pattern.
                            let contrib_tempo_item =
                                factor_contribution(per_item.tempo_factor, &stats_tempo, PlanFactor::TempoGain.signed(), w_tempo);
                            let tempo_delta = contrib_tempo_item - contrib_tempo_primary;

                            // per_item.considerations is the composite:
                            //   urgency / role_affinity        — from item-level (plan-agnostic,
                            //                                     set in build_agenda 11.3)
                            //   feasibility / leverage / safety /
                            //   continuation_value             — from OverlayConsiderationsStage
                            //                                     (plan-aware, 11.4)
                            // PickBest reads the composite only; item-level baseline lives in
                            // agenda.items[i].considerations for separate observability and is
                            // pulled into per_item by the overlay stage.
                            let cdot = per_item.considerations.weighted_dot(&band_weights);

                            // composed = initial + intent_delta + tempo_delta + W × cdot
                            let composed = ann.score_initial + intent_delta + tempo_delta + w_intent * cdot;

                            if best_composed.is_none_or(|(best, _)| composed > best) {
                                best_composed = Some((composed, item_idx as u8));
                            }
                        }

                        // Step 11.6: snapshot considerations for all per_item entries
                        // (serialised in schema v32 as considerations_per_item).
                        // Populated unconditionally so the log contains the full
                        // overlay even for non-chosen / ineligible items.
                        ann.considerations_per_item = ann
                            .per_item
                            .iter()
                            .map(|pi| pi.considerations)
                            .collect();

                        // Step 11.7: snapshot reject reasons alongside considerations.
                        ann.reject_reasons_per_item = ann
                            .per_item
                            .iter()
                            .map(|pi| pi.reject_reason)
                            .collect();

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

/// Apply deterministic, batch-scaled noise to every finite-score plan in the
/// pool before argmax. Replaces Pass 2 noise from `scorer.rs::finalize_scores`.
///
/// Returns a `Vec<f32>` (length `pool.len()`) with the accumulated noise
/// per plan (0.0 for skipped / masked plans). Mutates `pool.annotations[i].score`
/// in-place for finite scores.
///
/// If `s_min` or `s_max` is not finite (all plans masked), returns a zero vec
/// immediately — no fallback to a constant spread.
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
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::scoring::factors::PlanFactorValues;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, PoolBuilder,
        StageTestHarness, UnitBuilder,
        snapshot_from,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;

    // ── run_pick: no trace_base_eq_score — PickBest does not read score_trace ─

    fn run_pick(scores: Vec<f32>) -> ScoredPool {
        let n = scores.len();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(); n];
        let h = StageTestHarness::new(actor);
        let mut pool = PoolBuilder::new(plans)
            .scores(&scores)
            .build();
        h.run(|ctx| PickBestStage.apply(&mut pool, ctx));
        pool
    }

    #[test]
    fn pick_best_marks_exactly_one_chosen() {
        let pool = run_pick(vec![0.3, 0.8, 0.5]);
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");
    }

    #[test]
    fn pick_best_selects_highest_score() {
        // With deterministic DiceRng seed and no mercy margin (default difficulty),
        // the highest-scored plan should be chosen.
        let pool = run_pick(vec![0.1, 0.9, 0.4]);
        // Index 1 has the highest score.
        assert!(pool.annotations[1].chosen, "highest-scored plan should be chosen");
        assert!(pool.annotations[1].pick.is_some(), "chosen plan should have PickInfo");
    }

    #[test]
    fn pick_best_noop_on_empty_pool() {
        let pool = run_pick(vec![]);
        assert_eq!(pool.len(), 0);
    }

    // ── apply_pick_jitter tests ───────────────────────────────────────────────

    /// Build a pool with given scores and run apply_pick_jitter.
    /// Returns (noise_vec, post_scores) where post_scores[i] is score post-jitter.
    /// Kept inline: requires a custom DifficultyProfile — harness always uses default.
    fn run_jitter(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        difficulty: &DifficultyProfile,
    ) -> (Vec<f32>, Vec<f32>) {
        assert_eq!(plans.len(), scores.len());
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, s) in pool.annotations.iter_mut().zip(scores.iter()) {
            ann.score = *s;
        }
        let noise = apply_pick_jitter(&mut pool, &ctx);
        let post_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        (noise, post_scores)
    }

    /// score_noise = 0.0 (normal difficulty) → jitter returns all-zeros, scores unchanged.
    #[test]
    fn pick_jitter_no_op_when_noise_amp_zero() {
        let difficulty = DifficultyProfile::normal();
        assert_eq!(difficulty.score_noise(), 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        let scores = vec![0.1_f32, 0.5, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores.clone(), &difficulty);

        assert_eq!(noise, vec![0.0_f32; 3], "noise vec must be all zeros");
        assert_eq!(post_scores, scores, "scores must be unchanged");
    }

    /// Plans with score = -inf (masked) must have noise[i] = 0.0 and score unchanged.
    #[test]
    fn pick_jitter_skips_masked_plans() {
        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        // Middle plan is masked.
        let scores = vec![0.5_f32, f32::NEG_INFINITY, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores, &difficulty);

        assert_eq!(noise[1], 0.0, "masked plan noise must be zero");
        assert_eq!(post_scores[1], f32::NEG_INFINITY, "masked plan score must be unchanged");
        // Non-masked plans get non-zero noise (deterministic, may be any value).
        // Just verify they're finite.
        assert!(post_scores[0].is_finite(), "plan 0 score should be finite");
        assert!(post_scores[2].is_finite(), "plan 2 score should be finite");
    }

    /// Noise is order-invariant: same plan in position 0 or 1 gets the same noise value.
    /// Migrates the invariant tested in `scorer.rs::noise_is_plan_order_invariant`.
    #[test]
    fn pick_jitter_is_plan_order_invariant() {
        use crate::combat::ai::plan::types::{PlanStep, StepOutcome};

        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let pos_a = hex_from_offset(3, 0);
        let pos_b = hex_from_offset(2, 0);

        // Two distinct plans targeting different positions (different canonical hash).
        let mk_plan = |target_pos: crate::game::hex::Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: bevy::prelude::Entity::from_raw_u32(99).expect("valid"),
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let plan_a = mk_plan(pos_a);
        let plan_b = mk_plan(pos_b);

        let scores = vec![0.5_f32, 0.5];

        // Order AB.
        let (noise_ab, _) = run_jitter(vec![plan_a.clone(), plan_b.clone()], scores.clone(), &difficulty);
        // Order BA.
        let (noise_ba, _) = run_jitter(vec![plan_b.clone(), plan_a.clone()], scores.clone(), &difficulty);

        // noise_ab[0] = noise for plan_a; noise_ba[1] = noise for plan_a.
        assert_eq!(
            noise_ab[0], noise_ba[1],
            "plan_a noise must not depend on pool position",
        );
        assert_eq!(
            noise_ab[1], noise_ba[0],
            "plan_b noise must not depend on pool position",
        );
    }

    /// Winner's PickInfo.noise_applied is populated with the actual noise value
    /// when score_noise > 0.
    #[test]
    fn pick_jitter_records_noise_applied_in_pick_info() {
        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let n = 3;
        let plans = vec![TurnPlan::default(); n];
        let scores = [0.1_f32, 0.5, 0.3];

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, &difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, s) in pool.annotations.iter_mut().zip(scores.iter()) {
            ann.score = *s;
            ann.factors = PlanFactorValues::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);

        let winner = pool.annotations.iter().find(|a| a.chosen).expect("winner must exist");
        let pi = winner.pick.as_ref().expect("winner must have PickInfo");
        assert_ne!(pi.noise_applied, 0.0, "noise_applied must be non-zero under easy difficulty");
    }

    // ── Step 11.4: per-agenda-item composition tests ──────────────────────────

    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::outcome::PerItemEval;

    fn agenda_item_with_considerations(
        kind: IntentKind,
        considerations: IntentConsiderations,
    ) -> AgendaItem {
        AgendaItem {
            kind,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations,
        }
    }

    fn uniform_considerations() -> IntentConsiderations {
        IntentConsiderations {
            urgency: 1.0,
            feasibility: 1.0,
            leverage: 1.0,
            safety: 1.0,
            role_affinity: 1.0,
            continuation_value: 1.0,
        }
    }

    fn zero_considerations() -> IntentConsiderations {
        IntentConsiderations {
            urgency: 0.0,
            feasibility: 0.0,
            leverage: 0.0,
            safety: 0.0,
            role_affinity: 0.0,
            continuation_value: 0.0,
        }
    }

    /// Reconstruct the `w_intent` value that `PickBestStage` uses, mirroring
    /// the same path: default actor role weights × `intent_commitment`.
    /// Used to write exact-value assertions for cdot bonus.
    fn expected_w_intent() -> f32 {
        use crate::combat::ai::scoring::factors::{PlanFactor, StepFactor};
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut weights = if scoring.last_goal.is_some() {
            actor.role.factor_weights_continuation(world.tuning)
        } else {
            actor.role.factor_weights(world.tuning)
        };
        let slot = StepFactor::count() + PlanFactor::Intent as usize;
        weights[slot] *= world.difficulty.intent_commitment;
        weights[slot]
    }

    /// Build a pool with per_item data and run PickBest with an agenda.
    fn run_pick_with_agenda(
        pre_scores: Vec<f32>,
        score_initials: Vec<f32>,
        per_items: Vec<Vec<PerItemEval>>,
        agenda: &Agenda,
    ) -> ScoredPool {
        let n = pre_scores.len();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(); n];
        let mut h = StageTestHarness::new(actor);
        h.agenda = Some(agenda.clone());
        let mut pool = PoolBuilder::new(plans)
            .scores(&pre_scores)
            .score_initials(&score_initials)
            .per_items(per_items)
            .build();
        h.run(|ctx| PickBestStage.apply(&mut pool, ctx));
        pool
    }

    /// Single item, uniform considerations, item intent==primary intent (both 0).
    /// intent_delta = 0, tempo_delta = 0.
    /// composed = score_initial + 0 + 0 + w_intent × cdot.
    /// Test pins this explicit form (additive, not multiplicative).
    #[test]
    fn composition_collapses_to_base_when_considerations_uniform() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![agenda_item_with_considerations(
                IntentKind::Reposition,
                uniform_considerations(),
            )],
        };
        let initial = 0.75_f32;
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0, // same as primary (all factors default to 0)
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![initial],
            vec![initial],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "sole plan should be chosen");
        // Score must be finite and > score_initial because cdot > 0 with uniform considerations.
        let post_score = pool.annotations[0].score;
        assert!(post_score.is_finite(), "composed score must be finite");
        assert!(
            post_score >= initial,
            "composed = initial + W×cdot ≥ initial (cdot≥0), got {post_score} vs initial {initial}"
        );
    }

    /// Two plans, two items: argmax selects the item with highest composed score.
    /// Plan 1 has item 1 ineligible (FocusTarget mask); item 0 is eligible.
    /// Plan 0 has both items eligible; item 1 gives better cdot → attributed to item 1.
    #[test]
    fn multi_item_pick_attributes_to_winning_item() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(
                    IntentKind::Reposition,
                    IntentConsiderations { urgency: 0.1, feasibility: 1.0, leverage: 0.1, safety: 1.0, role_affinity: 0.1, continuation_value: 0.1 },
                ),
                agenda_item_with_considerations(
                    IntentKind::Reposition,
                    IntentConsiderations { urgency: 0.9, feasibility: 1.0, leverage: 0.9, safety: 1.0, role_affinity: 0.9, continuation_value: 0.9 },
                ),
            ],
        };
        // Plan 0 with both items eligible; item 1 has much higher considerations.
        let per_items = vec![
            vec![
                PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: IntentConsiderations { urgency: 0.1, feasibility: 1.0, leverage: 0.1, safety: 1.0, role_affinity: 0.1, continuation_value: 0.1 } },
                PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: IntentConsiderations { urgency: 0.9, feasibility: 1.0, leverage: 0.9, safety: 1.0, role_affinity: 0.9, continuation_value: 0.9 } },
            ],
        ];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "sole plan should be chosen");
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(1),
            "plan should be attributed to item 1 (higher cdot)"
        );
    }

    /// Empty agenda → legacy path: annotation.agenda_item stays None, chosen set normally.
    #[test]
    fn empty_agenda_falls_back_to_legacy_pipeline() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![], // empty
        };
        let pool = run_pick_with_agenda(
            vec![0.4, 0.9],
            vec![0.4, 0.9],
            vec![vec![], vec![]],  // empty per_item
            &agenda,
        );
        // Winner should be plan 1 (highest pre_score, no composition).
        assert!(pool.annotations[1].chosen, "highest-score plan should win in legacy path");
        // No agenda attribution.
        assert!(
            pool.annotations[1].agenda_item.is_none(),
            "agenda_item should be None in legacy (empty agenda) path"
        );
    }

    /// agenda_item attribution is written into the winning plan's annotation.
    #[test]
    fn agenda_item_attribution_persisted_in_annotation() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "plan should be chosen");
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(0),
            "agenda_item should be attributed to item index 0"
        );
    }


    // ── Step 11.4: new additive composition tests ─────────────────────────────

    /// Pins the main mathematical bug: two plans with different `score_initial`
    /// but identical `intent_factor`, `tempo_factor`, and `cdot` must produce
    /// composed scores that differ by exactly `score_initial_a - score_initial_b`.
    /// (The ratio bug would make the difference scale with score_initial.)
    #[test]
    fn item_score_does_not_scale_with_unrelated_base_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![agenda_item_with_considerations(
                IntentKind::Reposition,
                uniform_considerations(),
            )],
        };
        let same_per_item = PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        };

        // Plan A: initial=0.2, Plan B: initial=2.0. Identical intent/tempo/cdot.
        let pool = run_pick_with_agenda(
            vec![0.2, 2.0],
            vec![0.2, 2.0],
            vec![vec![same_per_item], vec![same_per_item]],
            &agenda,
        );
        // intent_delta = tempo_delta = 0 (same intent primary as item factor).
        // cdot is the same for both (same considerations + same band weights).
        // composed_A = 0.2 + w_intent*cdot
        // composed_B = 2.0 + w_intent*cdot
        // diff = 2.0 - 0.2 = 1.8 exactly.
        let score_a = pool.annotations[0].score;
        let score_b = pool.annotations[1].score;
        assert!(
            (score_b - score_a - 1.8_f32).abs() < 1e-4,
            "score diff must equal initial diff (1.8), got {}", score_b - score_a
        );
    }

    /// An ineligible item must be skipped; argmax chooses the next eligible item.
    #[test]
    fn ineligible_item_is_skipped_in_argmax() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let per_items = vec![vec![
            PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: zero_considerations() },
            PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: uniform_considerations() },
        ]];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen);
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(1),
            "ineligible item 0 must be skipped; argmax selects item 1"
        );
    }

    /// cdot bonus equals exactly `w_intent × weighted_dot(considerations, weights)`.
    /// Pins the additive formula by reconstructing w_intent the same way
    /// `PickBestStage` does and comparing to observed delta within 1e-4.
    #[test]
    fn cdot_changes_score_additively_with_intent_weight() {
        let agenda_zero = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
            ],
        };
        let agenda_full = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };

        let pool_zero = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            vec![vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true, reject_reason: None, considerations: zero_considerations() }]],
            &agenda_zero,
        );
        let pool_full = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            vec![vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true, reject_reason: None, considerations: uniform_considerations() }]],
            &agenda_full,
        );

        let cdot_delta = pool_full.annotations[0].score - pool_zero.annotations[0].score;
        let expected = expected_w_intent()
            * uniform_considerations().weighted_dot(&PriorityBand::NormalTactical.weights());

        assert!(
            (cdot_delta - expected).abs() < 1e-4,
            "cdot delta must equal w_intent × weighted_dot exactly: expected {expected}, got {cdot_delta}"
        );
    }

    /// A plan with a band-eligible item beats a fallback (no eligible items) plan
    /// when both start with the same initial score.
    #[test]
    fn attributed_plan_beats_fallback_plan_with_equal_initial_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        // Plan A: has one eligible item with uniform cdot.
        // Plan B: no eligible items → fallback (score stays at pipeline score = score_initial).
        let per_items = vec![
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: uniform_considerations() }],
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: uniform_considerations() }],
        ];
        let pool = run_pick_with_agenda(
            vec![0.5, 0.5],  // equal pipeline scores
            vec![0.5, 0.5],  // equal initials
            per_items,
            &agenda,
        );
        // Plan A: composed = 0.5 + 0 + 0 + w_intent*cdot  (cdot > 0 → composed > 0.5)
        // Plan B: fallback → score stays 0.5.
        // Plan A must win.
        assert!(
            pool.annotations[0].chosen,
            "attributed plan (eligible item) must beat fallback plan with equal initial score"
        );
    }

    /// A fallback plan with much higher initial score beats an attributed plan
    /// with low cdot. Pins that W×cdot is bounded and cannot override large signals.
    #[test]
    fn fallback_plan_can_beat_attributed_plan_with_low_cdot_when_initial_dominates() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
            ],
        };
        // Plan A: fallback (no eligible items), high initial = 2.0.
        // Plan B: attributed (cdot=0), initial = 1.0.
        //   composed_B = 1.0 + 0 + 0 + w_intent*0 = 1.0
        //   Plan A score = 2.0 (fallback, pipeline value)
        let per_items = vec![
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: zero_considerations() }],
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: zero_considerations() }],
        ];
        let pool = run_pick_with_agenda(
            vec![2.0, 1.0],
            vec![2.0, 1.0],
            per_items,
            &agenda,
        );
        assert!(
            pool.annotations[0].chosen,
            "fallback plan with initial=2.0 must beat attributed plan with initial=1.0 and cdot=0"
        );
    }

    /// Single-item agenda where item intent matches primary: intent_delta = 0,
    /// tempo_delta = 0, so composed must equal exactly `initial + W × cdot`.
    #[test]
    fn composed_equals_initial_plus_cdot_when_intent_matches_primary() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let initial = 0.4_f32;
        // item intent = primary intent = 0, tempo = 0 → both deltas = 0.
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![initial],
            vec![initial],
            per_items,
            &agenda,
        );
        let composed = pool.annotations[0].score;
        let expected = initial
            + expected_w_intent()
                * uniform_considerations().weighted_dot(&PriorityBand::NormalTactical.weights());
        assert!(
            (composed - expected).abs() < 1e-4,
            "composed must equal initial + W × cdot exactly: expected {expected}, got {composed}"
        );
    }

    /// Intent delta is the same for two plans with different score_initial
    /// but identical per-item intent values. Pins additive (not multiplicative) intent scaling.
    #[test]
    fn intent_delta_is_identical_for_different_base_scores() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        // Both plans: item intent_factor = 1.0 (different from primary which is 0.0).
        // Batch stats: both plans have intent_factor=1.0 → stats_intent from per_item.
        // But plan A initial=0.2, plan B initial=2.0.
        // intent_delta = factor_contrib(1.0, stats, signed, w) - factor_contrib(0.0, stats, signed, w).
        // Same for both since factors are identical.
        let same_per_item = PerItemEval {
            intent_factor: 1.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        };
        let pool = run_pick_with_agenda(
            vec![0.2, 2.0],
            vec![0.2, 2.0],
            vec![vec![same_per_item], vec![same_per_item]],
            &agenda,
        );
        let score_a = pool.annotations[0].score;
        let score_b = pool.annotations[1].score;
        // composed_A = 0.2 + intent_delta + W*cdot
        // composed_B = 2.0 + intent_delta + W*cdot
        // diff = 1.8 (intent_delta cancels out — same for both plans).
        assert!(
            (score_b - score_a - 1.8_f32).abs() < 1e-4,
            "intent_delta must be identical for both plans; diff must equal initial diff (1.8), got {}",
            score_b - score_a
        );
    }

    /// With non-zero noise and two plans with equal pre-noise score, the winner
    /// is determined by the jitter — not by insertion order. Jitter runs before argmax.
    #[test]
    fn pipeline_pick_runs_jitter_before_argmax() {
        use crate::combat::ai::plan::types::{PlanStep, StepOutcome};

        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let pos_a = hex_from_offset(3, 0);
        let pos_b = hex_from_offset(2, 0);

        let mk_plan = |target_pos: crate::game::hex::Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: bevy::prelude::Entity::from_raw_u32(99).expect("valid"),
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let plan_a = mk_plan(pos_a);
        let plan_b = mk_plan(pos_b);

        let pre_noise_score = 0.5_f32;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, &difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(vec![plan_a, plan_b]);
        for ann in pool.annotations.iter_mut() {
            ann.score = pre_noise_score;
            ann.factors = PlanFactorValues::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);

        // Exactly one winner.
        let chosen: Vec<usize> = pool.annotations.iter().enumerate()
            .filter(|(_, a)| a.chosen)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(chosen.len(), 1, "exactly one plan chosen");

        // Winner must have non-zero noise_applied (jitter ran).
        let winner = &pool.annotations[chosen[0]];
        let pi = winner.pick.as_ref().expect("winner has PickInfo");
        assert_ne!(pi.noise_applied, 0.0, "noise_applied must reflect jitter contribution");
    }

    // ── commit_plan tests (consolidated from planning/picker.rs) ──────────────

    mod commit_plan_tests {
        use super::*;
        use crate::core::AbilityId;
        use crate::game::hex::hex_from_offset;

        fn ent(id: u32) -> Entity {
            Entity::from_raw_u32(id).expect("valid")
        }

        fn plan_from(steps: Vec<PlanStep>) -> TurnPlan {
            TurnPlan {
                steps,
                final_pos: hex_from_offset(0, 0),
                residual_ap: 0,
                residual_mp: 0,
                outcomes: Vec::new(),
                partial_score: 0.0,
                sim_snapshots: Vec::new(),
                annotation: Default::default(),
            }
        }

        #[test]
        fn commit_empty_plan_ends_turn() {
            let (decision, consumed) = commit_plan(&plan_from(vec![]), hex_from_offset(0, 0));
            assert!(matches!(decision, AiDecision::EndTurn));
            assert_eq!(consumed, 0);
        }

        #[test]
        fn commit_solo_cast_consumes_one() {
            let plan = plan_from(vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(1),
                target_pos: hex_from_offset(0, 0),
            }]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert!(matches!(decision, AiDecision::CastInPlace { .. }));
            assert_eq!(consumed, 1);
        }

        #[test]
        fn commit_move_cast_bundles_into_two() {
            let plan = plan_from(vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Cast {
                    ability: AbilityId::from("strike"),
                    target: ent(2),
                    target_pos: hex_from_offset(2, 0),
                },
            ]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            match decision {
                AiDecision::MoveAndCast { path, ability, target, .. } => {
                    assert_eq!(path.len(), 1);
                    assert_eq!(ability.0, "strike");
                    assert_eq!(target, ent(2));
                }
                other => panic!("expected MoveAndCast, got {:?}", std::mem::discriminant(&other)),
            }
            assert_eq!(consumed, 2);
        }

        #[test]
        fn commit_solo_move_consumes_one() {
            let plan = plan_from(vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert!(matches!(
                decision,
                AiDecision::Move { origin: MoveOrigin::BestPlan, .. }
            ));
            assert_eq!(consumed, 1);
        }

        #[test]
        fn commit_move_move_keeps_first_only_no_bundle() {
            let plan = plan_from(vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Move { path: vec![hex_from_offset(2, 0)] },
            ]);
            let (_, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert_eq!(consumed, 1, "Move→Move does not bundle");
        }
    }
}
