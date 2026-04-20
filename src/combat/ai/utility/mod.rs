//! Top-level AI decision pipeline.
//!
//! Layout:
//! - `fallback` — moves used when no plan candidates survive beam search.
//!
//! Plan generation, scoring, sanity adjustment and final pick live in
//! `combat::ai::planning`. This module wires them together.

#![allow(clippy::too_many_arguments)]

mod fallback;

pub use crate::combat::ai::planning::PickMechanics;

use crate::content::content_view::ContentView;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    default_focus_target, intent_viability_threshold, select_intent, update_memory,
    AiMemory, TacticalIntent,
};
use crate::combat::ai::log::{self, AiLogger, IntentBlock};
use crate::combat::ai::planning::{
    apply_protect_self_mask, commit_plan, generate_plans, pick_best_plan,
    record_committed_reservations, rescore_with_intent, sanity_adjust_plans,
    score_plans_with_raw,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::content::abilities::CasterContext;
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng};
use crate::game::components::Abilities;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// ── Public types ────────────────────────────────────────────────────────────

pub enum AiDecision {
    CastInPlace {
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveAndCast {
        path: Vec<Hex>,
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    /// Pure movement (no cast bundled). `origin` records whether this came
    /// from `commit_plan` (best plan after scoring) or from `fallback_move`
    /// (no plans survived beam search). Runtime handling is identical — the
    /// distinction only labels debug/log output.
    Move {
        path: Vec<Hex>,
        origin: MoveOrigin,
    },
    EndTurn,
}

/// Source of a `Move` decision. See `AiDecision::Move`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveOrigin {
    /// Came out of `picker::commit_plan` — winning plan's first step is a
    /// move-only prefix. Historically labelled "MoveOnlyRetreat".
    BestPlan,
    /// Came out of `utility::fallback::fallback_move` — no plans were
    /// generated. Historically labelled "MoveCloser".
    Fallback,
}

// ── Context ─────────────────────────────────────────────────────────────────
//
// The AI chain takes three categories of data. They used to live side-by-side
// as a flat `UtilityContext`; splitting by lifetime/scope makes every caller
// honestly declare what it touches.
//
// - `AiWorld`   — static for the whole combat (content, difficulty).
// - `ActorCtx`  — per-actor: who is casting + caster-specific scoring params.
// - `blocked_tiles` — per-turn pathfinding plumbing. Used only at the BFS
//   boundary (`generate_plans`, `fallback_move`); passed as a plain `&HashSet`
//   next to those entry points, not buried in the ctx.
//
// `UtilityContext` stays as a thin composite so the deep scoring/generation
// chain keeps a single parameter, but it no longer mixes lifetimes.

/// World-scope data. Stable for the entire combat.
pub struct AiWorld<'a> {
    pub content: &'a ContentView,
    pub difficulty: &'a DifficultyProfile,
}

/// Per-actor data rebuilt each AI tick. Caster mods, ability list, crit-fail
/// profile. `crit_fail_chance` is derived from global settings rather than
/// the actor, but it pairs with `crit_fail_effect` everywhere it's read
/// (always inside `crit_fail_adjusted`), so it lives here to keep the pair
/// together.
pub struct ActorCtx<'a> {
    pub caster: &'a CasterContext,
    pub abilities: &'a Abilities,
    pub crit_fail_effect: CritFailEffect,
    pub crit_fail_chance: f32,
}

/// Composite threaded through scoring/generation/sanity. Splits into
/// `world` (static) and `actor` (per-tick) so every call site declares which
/// half it needs, while still passing one parameter.
pub struct UtilityContext<'a> {
    pub world: AiWorld<'a>,
    pub actor: ActorCtx<'a>,
}

/// Bundle of every read-only context the scoring layer touches. Replaces
/// the 5-7 parameter signatures (active, ctx, snap, maps, reservations) that
/// used to thread through every factor / plan / picker function.
///
/// Two lifetime parameters because perspective `(active, snap)` can be swapped
/// mid-plan when scoring against a sim'd state — `with_perspective` returns
/// a fresh `ScoringCtx` reusing the world refs but with a shorter `'p`.
pub struct ScoringCtx<'w, 'p> {
    pub utility: &'w UtilityContext<'w>,
    pub maps: &'w InfluenceMaps,
    pub reservations: &'w Reservations,
    pub snap: &'p BattleSnapshot,
    pub active: &'p UnitSnapshot,
}

impl<'w, 'p> ScoringCtx<'w, 'p> {
    /// Borrow the world refs + override perspective. Used when scoring a
    /// plan step against the cached pre-step sim snapshot (perspective shifts
    /// to the simulated actor at that moment).
    pub fn with_perspective<'q>(
        &self,
        active: &'q UnitSnapshot,
        snap: &'q BattleSnapshot,
    ) -> ScoringCtx<'w, 'q> {
        ScoringCtx {
            utility: self.utility,
            maps: self.maps,
            reservations: self.reservations,
            snap,
            active,
        }
    }
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Top-level decision function. Replaces evaluate_targets + plan_movement.
/// When `debug` is true, returns an `AiDebugSnapshot` alongside the decision.
pub fn pick_action(
    actor: Entity,
    actor_pos: Hex,
    ctx: &UtilityContext,
    blocked_tiles: &HashSet<Hex>,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    rng: &mut DiceRng,
    memory: &mut AiMemory,
    reservations: &mut Reservations,
    logger: &mut AiLogger,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>) {
    let log_on = logger.is_enabled();
    let t0 = if log_on { Some(std::time::Instant::now()) } else { None };

    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None);
    };

    // ── Select tactical intent ──────────────────────────────────────────
    let choice = select_intent(active, snap, maps, memory, ctx.world.difficulty);
    update_memory(memory, &choice.intent);
    let mut intent = choice.intent;
    let mut intent_reason = choice.reason;

    // ── Generate plans (beam search over depths) ───────────────────────
    let plans = generate_plans(actor, ctx, blocked_tiles, snap, maps);

    if plans.is_empty() {
        let decision = fallback::fallback_move(active, blocked_tiles, snap, maps);
        let ds = if debug {
            Some(build_fallback_debug(
                active, actor_pos, &intent, &intent_reason, &decision,
                "no plans generated", snap, maps, debug_names,
            ))
        } else { None };
        return (decision, ds);
    }

    // Bundle the read-only scoring inputs once. Threaded as `&ScoringCtx`
    // through scorer + factors + sanity instead of the old 5-7 individual
    // refs per call.
    let scoring_ctx = ScoringCtx {
        utility: ctx,
        maps,
        reservations,
        snap,
        active,
    };

    // ── Score plans under the chosen intent ────────────────────────────
    // Keep the raw-factor matrix for logging even after rescoring under a
    // fallback intent below; it's cheap to recompute and represents what the
    // winning decision was actually scored against.
    let (mut scored, mut raw_factors) =
        score_plans_with_raw(&plans, &intent, &scoring_ctx, rng);

    // ── Intent viability guard ─────────────────────────────────────────
    // If no plan achieves the intent's signal, fall back. Two tiers:
    //   - midpanic: HP below midpanic_hp_threshold AND standing in danger →
    //     `ProtectSelf`. The actor can't execute the original intent *and*
    //     is too exposed to blindly push toward a fallback focus target.
    //   - default: reachable `FocusTarget` over a live enemy, same as before.
    // Plan generation is intent-agnostic, so rescoring against the same pool
    // is enough.
    if let Some(threshold) = intent_viability_threshold(&intent) {
        // Per-plan intent_sum produced by `compute_plan_intent_sum`; the max
        // over plans answers "can any candidate plan realistically execute
        // the chosen intent?"
        let max_align = raw_factors
            .iter()
            .map(|f| f.intent)
            .fold(f32::NEG_INFINITY, f32::max);
        if max_align < threshold {
            let hp_pct = active.hp_pct();
            let actor_danger = maps.danger.get(active.pos);
            let midpanic_hp = ctx.world.difficulty.midpanic_hp_threshold();
            let panic_danger = ctx.world.difficulty.awareness_danger_threshold();
            let midpanic = hp_pct < midpanic_hp && actor_danger > panic_danger;

            let new_intent = if midpanic {
                intent_reason = format!(
                    "midpanic_fallback: hp%={:.0}%<{:.0}% AND danger={:.2}>{:.2} (max_align={:.2}<{:.2})",
                    hp_pct * 100.0, midpanic_hp * 100.0,
                    actor_danger, panic_danger,
                    max_align, threshold,
                );
                Some(TacticalIntent::ProtectSelf)
            } else {
                let exclude = match &intent {
                    TacticalIntent::FocusTarget { target } => Some(*target),
                    _ => None,
                };
                default_focus_target(active, snap, &plans, actor_pos, exclude).map(|t| {
                    let original_label = format!("{:?}", intent.kind());
                    intent_reason = format!(
                        "fallback from {}: max_align={:.2}<threshold={:.2}",
                        original_label, max_align, threshold,
                    );
                    TacticalIntent::FocusTarget { target: t }
                })
            };

            if let Some(new) = new_intent {
                if intent.kind() != new.kind() || intent.target() != new.target() {
                    intent = new;
                    // Reuse the non-intent factor columns; only the intent
                    // column (factor[7]) depends on the chosen intent.
                    scored = rescore_with_intent(
                        &plans, &mut raw_factors, &intent, &scoring_ctx, rng,
                    );
                }
            }
        }
    }

    // Sanity adjust: multiplicative penalties for situations the 9-factor
    // score can't catch (low-HP through AoO corridors, self-AoE, LOS
    // blindspots, retreat traps). Runs on all plans so low-ranked terrible
    // ones can't sneak up via noise.
    sanity_adjust_plans(&mut scored, &plans, &scoring_ctx);

    // ProtectSelf mask: when intent is (or fell to) ProtectSelf, mask any
    // plan whose first step isn't defensive to -∞. This is where the intent
    // gets real teeth — without it, "I want to protect myself" is just a
    // +1.0 intent factor on a few candidates, easily out-scored by high-
    // damage offensive plans. If no plan is defensive (surrounded, no safe
    // move), LastStand rescoring takes over so the actor at least lands a
    // final useful hit.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let margin = ctx.world.difficulty.defensive_tile_margin();
        let any_defensive = apply_protect_self_mask(
            &mut scored, &plans, active, ctx.world.content, maps, margin,
        );
        if !any_defensive {
            let last_stand = TacticalIntent::LastStand;
            // Same reuse as the viability fallback: intent-independent
            // factors stay; only factor[7] refreshes under LastStand.
            scored = rescore_with_intent(
                &plans, &mut raw_factors, &last_stand, &scoring_ctx, rng,
            );
            intent_reason = format!("{intent_reason} → LastStand (no defensive plan)");
        }
    }

    // Pick best plan via mercy + top-K window (same math as single-candidate
    // pick). `raw_factors` is threaded in so mercy_cruelty reads the
    // precomputed kill/cc columns instead of recomputing plan factors per
    // window slot. `PickMechanics` is ~24B of stack for ≤3 pool entries —
    // cheap enough to always collect; debug overlay reads it, prod ignores.
    let (best_idx, mech) = pick_best_plan(&scored, &raw_factors, ctx, rng);
    let pick_mech = debug.then_some(mech);

    let best_plan = &plans[best_idx];
    let (decision, consumed) = commit_plan(best_plan, actor_pos);

    // Debug formatter walks the plans directly — one `ScoredStep` per plan
    // representing the committed first-tick action.
    let debug_snapshot = if debug {
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &intent_reason, &plans, &scored,
            &raw_factors, &decision, snap, maps, debug_names, pick_mech.as_ref(),
        ))
    } else {
        None
    };

    // Reserve only the prefix that will actually execute this tick — every
    // subsequent tick re-plans from scratch, so reserving the full plan
    // would leave ghost reservations on actions that never happen.
    record_committed_reservations(
        best_plan, consumed, active, ctx, snap, reservations, actor_pos,
    );

    // ── AI log: write structured entry for offline analysis ────────────
    if log_on {
        let decision_time_ms = t0.map_or(0, |t| t.elapsed().as_millis() as u64);
        let plan_id = logger.next_plan_id();

        // Rank plans by final score, keep top-10 for size budget.
        let mut indexed: Vec<(usize, f32)> =
            scored.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let shown = indexed.len().min(10);
        let plan_entries: Vec<_> = indexed
            .iter()
            .take(shown)
            .enumerate()
            .map(|(rank, &(idx, score))| {
                log::plan_to_log_entry(
                    &plans[idx],
                    rank + 1,
                    idx == best_idx,
                    raw_factors[idx].as_array(),
                    score,
                )
            })
            .collect();

        let actor_name = debug_names
            .get(&actor)
            .map(|s| s.as_str())
            .unwrap_or("<unknown>");
        let intent_block = IntentBlock {
            intent: &intent,
            selection_kind: log::classify_selection(&intent_reason),
            reason_text: &intent_reason,
        };
        let entry = log::build_entry(
            plan_id, decision_time_ms, active, actor_name, snap, intent_block,
            plans.len(), shown, plan_entries, &decision,
        );
        if let Err(e) = logger.write_entry(&entry) {
            warn!("AI log write failed: {}", e);
        }
    }

    (decision, debug_snapshot)
}
