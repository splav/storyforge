//! Top-level AI decision pipeline.
//!
//! Layout:
//! - `pick` — final top-K + mercy candidate selection, post-pick reservations.
//! - `sanity` — multiplicative penalties for dangerous/bad candidates + defensive classification.
//! - `fallback` — moves used when no cast candidates survive.

#![allow(clippy::too_many_arguments)]

mod fallback;
mod pick;
mod sanity;

pub use pick::PickMechanics;

pub use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};

use crate::content::content_view::ContentView;
use crate::combat::ai::candidates::generate_candidates;
use crate::combat::ai::constraints::filter_candidates;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::factors::score_candidates;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    default_focus_target, intent_score, intent_viability_threshold, select_intent, update_memory,
    AiMemory, TacticalIntent,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::content::abilities::CasterContext;
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{Abilities, Team};
use crate::game::hex::Hex;
use crate::game::pathfinding::ReachableMap;
use bevy::prelude::*;
use std::collections::HashMap;

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
    MoveCloser {
        path: Vec<Hex>,
    },
    MoveOnlyRetreat {
        path: Vec<Hex>,
    },
    EndTurn,
}

// ── Context ─────────────────────────────────────────────────────────────────

pub struct UtilityContext<'a> {
    pub content: &'a ContentView,
    pub difficulty: &'a DifficultyProfile,
    pub caster: &'a CasterContext,
    pub abilities: &'a Abilities,
    pub opponent_team: Team,
    pub crit_fail_effect: CritFailEffect,
    pub crit_fail_chance: f32,
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Top-level decision function. Replaces evaluate_targets + plan_movement.
/// When `debug` is true, returns an `AiDebugSnapshot` alongside the decision.
pub fn pick_action(
    actor: Entity,
    actor_pos: Hex,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
    rng: &mut DiceRng,
    memory: &mut AiMemory,
    reservations: &mut Reservations,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>) {
    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None);
    };

    // ── Select tactical intent ──────────────────────────────────────────
    let choice = select_intent(active, snap, maps, memory, ctx.difficulty);
    update_memory(memory, &choice.intent);
    let mut intent = choice.intent;
    let mut intent_reason = choice.reason;

    // ── Generate candidates ─────────────────────────────────────────────
    let mut candidates = generate_candidates(actor_pos, active, ctx, snap, maps, reach);

    if candidates.is_empty() {
        let decision = fallback::fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &intent_reason, &decision, "no candidates generated", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Hard constraints ────────────────────────────────────────────────
    filter_candidates(&mut candidates, active, snap, maps, ctx.content);

    if candidates.is_empty() {
        let decision = fallback::fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(active, actor_pos, &intent, &intent_reason, &decision, "all filtered by constraints", ctx, snap, maps, debug_names))
        } else { None };
        return (decision, ds);
    }

    // ── Utility scoring ─────────────────────────────────────────────────
    let mut scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, reservations, rng);

    // ── Intent viability guard ─────────────────────────────────────────
    // If the chosen intent can't be executed by any candidate (e.g., Reposition
    // with no tile actually improving, FocusTarget on an unreachable enemy),
    // fall back to FocusTarget over a reachable enemy and rescore.
    if let Some(threshold) = intent_viability_threshold(&intent) {
        let max_align = candidates
            .iter()
            .map(|c| intent_score(&intent, c, active, snap, maps, ctx.content, ctx.difficulty))
            .fold(f32::NEG_INFINITY, f32::max);
        if max_align < threshold {
            // Skip the original target if it was an unreachable FocusTarget —
            // otherwise the "fallback" picks the same target and does nothing.
            let exclude = match &intent {
                TacticalIntent::FocusTarget { target } => Some(*target),
                _ => None,
            };
            if let Some(default_target) = default_focus_target(active, snap, &candidates, exclude) {
                let new_intent = TacticalIntent::FocusTarget { target: default_target };
                // Only log + rescore if something actually changed.
                if intent.kind() != new_intent.kind() || intent.target() != new_intent.target() {
                    let original_label = format!("{:?}", intent.kind());
                    // Rebuild reason with the real numbers that made the guard fire —
                    // no separate "explain" path to drift from this.
                    intent_reason = format!(
                        "fallback from {}: max_align={:.2}<threshold={:.2}",
                        original_label, max_align, threshold,
                    );
                    intent = new_intent;
                    scored = score_candidates(&candidates, active, &intent, ctx, snap, maps, reservations, rng);
                }
            }
        }
    }

    // ── Sanity check on top candidates ─────────────────────────────────
    sanity::sanity_adjust(&mut scored, &candidates, active, snap, maps, ctx);

    // ── ProtectSelf: mask non-defensive candidates so pick picks safety ──
    // Retreat is already a first-class MoveOnly candidate in the pool, so no
    // separate retreat branch — the top candidate after masking is either a
    // defensive cast (self-heal) or a safe MoveOnly tile.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let current_danger = maps.danger.get(active.pos);
        let def_margin = ctx.difficulty.defensive_tile_margin();
        let mut any_defensive = false;
        for (i, s) in scored.iter_mut().enumerate() {
            if sanity::is_defensive(&candidates[i], current_danger, ctx.content, maps, def_margin) {
                any_defensive = true;
            } else {
                *s = f32::NEG_INFINITY;
            }
        }
        // No viable survival option → LastStand: re-score for maximum impact.
        if !any_defensive {
            let last_stand = TacticalIntent::LastStand;
            scored = score_candidates(&candidates, active, &last_stand, ctx, snap, maps, reservations, rng);
        }
    }

    // Pick best: combine mercy (soft shift toward gentler options) and
    // top_k (random pick among top-K, controlled by decision_quality).
    let (best_idx, pick_mech) = pick::pick_best_candidate(
        &scored, &candidates, active, &intent, ctx, snap, maps, reservations, rng,
    );

    // Build debug snapshot before swap_remove invalidates indices.
    let debug_snapshot = if debug {
        let best = &candidates[best_idx];
        let decision_preview = pick::decision_from_candidate(best, actor, actor_pos);
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &intent_reason, &candidates, &scored, &decision_preview,
            ctx, snap, maps, reservations, debug_names, Some(&pick_mech),
        ))
    } else {
        None
    };

    // ── Record reservations for subsequent units ─────────────────────
    {
        let best = &candidates[best_idx];
        pick::record_reservation(best, active, ctx, snap, reservations, actor_pos);
    }

    let best = candidates.swap_remove(best_idx);
    let decision = pick::decision_from_candidate(&best, actor, actor_pos);

    (decision, debug_snapshot)
}
