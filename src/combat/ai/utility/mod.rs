//! Top-level AI decision pipeline.
//!
//! Layout:
//! - `fallback` — moves used when no plan candidates survive beam search.
//! - `ranking`  — `PlanRanking` state + phase methods (viability, sanity,
//!   protect-self, pick) that `pick_action` walks in sequence.
//!
//! Plan generation, scoring, sanity adjustment and final pick live in
//! `combat::ai::planning`. This module wires them together.

#![allow(clippy::too_many_arguments)]

mod fallback;
mod ranking;

pub use crate::combat::ai::planning::PickMechanics;

use crate::content::content_view::ContentView;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    select_intent, update_memory, AiMemory, IntentReason, TacticalIntent,
};
use crate::combat::ai::log::{self, AiLogger, IntentBlock, TradeBlock};
use crate::combat::ai::factors::PlanFactors;
use crate::combat::ai::planning::{
    commit_plan, generate_plans, record_committed_reservations, TurnPlan,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::core::{AbilityId, DiceRng};
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::HashMap;

use ranking::PlanRanking;

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
// The AI chain reads two categories of data. After arch E (per-actor data
// migrated onto `UnitSnapshot`), only the world-scope half remains as a
// shared context — every per-actor fact is on the actor's snapshot row.
//
// - `AiWorld` — content + difficulty + combat-wide rules (crit_fail_chance).
//   Stable for the entire combat.
// - per-actor data lives on `UnitSnapshot` directly (caster_ctx,
//   crit_fail_effect, abilities). The scoring layer reads it through
//   `ScoringCtx.active`.
//
// Pathfinding stop-blockers come from `BattleSnapshot` directly — corpses
// are hp=0 units, no separate `blocked_tiles` channel.

/// World-scope data. Stable for the entire combat.
///
/// `crit_fail_chance` is a combat-wide rule (one die per combat, player +
/// AI pay the same odds) — sits alongside `content` and `difficulty` as
/// "how this world works for every actor".
pub struct AiWorld<'a> {
    pub content: &'a ContentView,
    pub difficulty: &'a DifficultyProfile,
    pub crit_fail_chance: f32,
}

/// Bundle of every read-only context the scoring layer touches. Replaces
/// the 5-7 parameter signatures (active, ctx, snap, maps, reservations) that
/// used to thread through every factor / plan / picker function.
///
/// Two lifetime parameters because perspective `(active, snap)` can be swapped
/// mid-plan when scoring against a sim'd state — `with_perspective` returns
/// a fresh `ScoringCtx` reusing the world refs but with a shorter `'p`.
pub struct ScoringCtx<'w, 'p> {
    pub world: &'w AiWorld<'w>,
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
            world: self.world,
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
    world: &AiWorld,
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
    let choice = select_intent(active, snap, maps, memory, world.difficulty);
    update_memory(memory, &choice.intent);

    // ── Generate plans (beam search over depths) ───────────────────────
    let plans = generate_plans(actor, world, snap, maps);

    if plans.is_empty() {
        let decision = fallback::fallback_move(active, snap, maps);
        let ds = if debug {
            Some(build_fallback_debug(
                active, actor_pos, &choice.intent, &choice.reason, &decision,
                "no plans generated", snap, maps, debug_names,
            ))
        } else { None };
        return (decision, ds);
    }

    // Bundle the read-only scoring inputs once. Threaded as `&ScoringCtx`
    // through scorer + factors + sanity instead of the old 5-7 individual
    // refs per call.
    let scoring_ctx = ScoringCtx {
        world,
        maps,
        reservations,
        snap,
        active,
    };

    // ── Phase pipeline ─────────────────────────────────────────────────
    // `PlanRanking` owns (intent, reason, scored, raw_factors) and each
    // phase method mutates them coherently. The pick_action body reads as
    // a linear sequence of phases — behavior-sensitive logic lives in the
    // methods and is unit-tested there.
    let mut ranking = PlanRanking::initial(&plans, choice.intent, choice.reason, &scoring_ctx);
    ranking.apply_viability(&plans, actor_pos, &scoring_ctx);
    ranking.apply_sanity(&plans, &scoring_ctx);
    // Snapshot post-sanity scores as the "base" — the value each plan had
    // immediately before adaptation rescored any of them. The log stores
    // both numbers per plan (v6 schema) so offline diagnostics can tell
    // "did adaptation move this rank?" without rerunning the pipeline.
    let base_scored = ranking.scored.clone();
    ranking.apply_adaptation(&plans, &scoring_ctx);
    if matches!(ranking.intent, TacticalIntent::ProtectSelf) {
        ranking.apply_protect_self(&plans, &scoring_ctx);
    }

    let (best_idx, mech) = ranking.pick(world, rng);

    // If adaptation switched the chosen plan's evaluation regime, wrap
    // the intent reason so logs/debug see the full chain
    // (prior_intent_reason → adapted_via X). The global intent itself is
    // left intact — the plan that won may not be the actor's "tactical
    // wish", and conflating the two confuses debug output.
    if let Some(adapt_reason) = ranking.adaptation.reasons.get(best_idx).and_then(|r| r.clone()) {
        let prior = std::mem::replace(&mut ranking.intent_reason, IntentReason::NoRuleDefault);
        ranking.intent_reason = IntentReason::Adapted {
            prior: Box::new(prior),
            reason: adapt_reason,
        };
    }
    let pick_mech = debug.then_some(mech);

    let best_plan = &plans[best_idx];
    let (decision, consumed) = commit_plan(best_plan, actor_pos);

    // Debug formatter walks the plans directly — one `ScoredStep` per plan
    // representing the committed first-tick action.
    let debug_snapshot = if debug {
        Some(build_debug_snapshot(
            active, actor_pos, &ranking.intent, &ranking.intent_reason, &plans,
            &ranking.scored, &ranking.raw_factors, &decision, snap, maps,
            debug_names, pick_mech.as_ref(),
        ))
    } else {
        None
    };

    // Reserve only the prefix that will actually execute this tick — every
    // subsequent tick re-plans from scratch, so reserving the full plan
    // would leave ghost reservations on actions that never happen.
    record_committed_reservations(
        best_plan, consumed, active, world, snap, reservations, actor_pos,
    );

    if log_on {
        let decision_time_ms = t0.map_or(0, |t| t.elapsed().as_millis() as u64);
        write_decision_log(
            logger, decision_time_ms, actor, active, snap, world.content,
            &ranking.intent, &ranking.intent_reason, &plans, &base_scored,
            &ranking.scored, &ranking.raw_factors, &ranking.adaptation,
            best_idx, &decision, debug_names,
        );
    }

    (decision, debug_snapshot)
}

/// Write one JSONL entry for the decision that `pick_action` just produced.
/// Kept out of `pick_action` so the hot path stays focused on decisioning;
/// the top-K sort and string formatting only run when logging is enabled.
fn write_decision_log(
    logger: &mut AiLogger,
    decision_time_ms: u64,
    actor: Entity,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    content: &ContentView,
    intent: &TacticalIntent,
    intent_reason: &IntentReason,
    plans: &[TurnPlan],
    base_scored: &[f32],
    scored: &[f32],
    raw_factors: &[PlanFactors],
    adaptation: &crate::combat::ai::planning::Adaptation,
    best_idx: usize,
    decision: &AiDecision,
    debug_names: &HashMap<Entity, String>,
) {
    let plan_id = logger.next_plan_id();

    // Cache once — actor's own value is plan-independent. Same
    // denominator the scorer used for its trade bonus, so the `score`
    // column the log reports matches the increment the scorer applied.
    let actor_value = crate::combat::ai::trade::unit_value(active, content);

    // Rank plans by final (adapted) score, keep top-10 for size budget.
    let mut indexed: Vec<(usize, f32)> =
        scored.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let shown = indexed.len().min(10);
    let plan_entries: Vec<_> = indexed
        .iter()
        .take(shown)
        .enumerate()
        .map(|(rank, &(idx, score))| {
            let br = crate::combat::ai::trade::trade_delta(
                &plans[idx], active, snap, content,
            );
            let trade = TradeBlock {
                delta: br.delta,
                killed: br.killed_value,
                lost: br.lost_value,
                self_lost: br.self_lost,
                self_lethal: br.self_lethal,
                score: crate::combat::ai::trade::trade_score(&br, actor_value),
            };
            log::plan_to_log_entry(
                &plans[idx],
                rank + 1,
                idx == best_idx,
                raw_factors[idx].as_array(),
                base_scored[idx],
                score,
                &adaptation.modes[idx],
                adaptation.reasons[idx].as_ref(),
                trade,
            )
        })
        .collect();

    let actor_name = debug_names
        .get(&actor)
        .map(|s| s.as_str())
        .unwrap_or("<unknown>");
    let reason_text = intent_reason.to_string();
    let intent_block = IntentBlock {
        intent,
        selection_kind: intent_reason.code(),
        reason_text: &reason_text,
        reason: intent_reason,
    };
    let entry = log::build_entry(
        plan_id, decision_time_ms, active, actor_name, snap, intent_block,
        plans.len(), shown, plan_entries, decision,
    );
    if let Err(e) = logger.write_entry(&entry) {
        warn!("AI log write failed: {}", e);
    }
}
