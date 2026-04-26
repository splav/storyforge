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

use crate::combat::ai::pipeline::{
    stages::{
        adaptation::AdaptationStage,
        killable_gate::KillableGateStage,
        protect_self::ProtectSelfMaskStage,
        repair_affinity::RepairAffinityStage,
        sanity::SanityStage,
        viability::ViabilityStage,
    },
    Pipeline, ScoredPool, StageCtx,
};
use crate::content::content_view::ContentView;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::tuning::AiTuning;
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

/// Structured output of a successful `pick_action` call (non-fallback path).
/// Carries the winning plan and its final score so `run_ai_turn` can store
/// them in `AiMemory` for the plan-freeze continuation logic.
pub struct ChosenInfo {
    /// Winning plan, without `sim_snapshots` (cleared to avoid carrying heavy
    /// simulation data across ticks — those are only needed during scoring).
    pub plan: TurnPlan,
    /// Final adapted score (post-mercy, post-adaptation) — the value used for
    /// the pick decision and written to the decision log.
    pub score: f32,
    /// Tactical intent that was active when the plan was scored.
    pub intent: TacticalIntent,
    /// Reason for the intent selection (post-adaptation). Used by
    /// `classify_continuation_outcome` to distinguish reactive vs voluntary
    /// goal abandons (step 6.6b).
    pub reason: IntentReason,
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
    pub tuning: &'a AiTuning,
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
    /// Need signals computed once per actor in `pick_action`; carried through
    /// scoring/factors/intent. Step 3.0 fills with zeros (`Default::default()`);
    /// step 3.1 fills via `appraisal::compute_need_signals`. Owned (Copy) —
    /// small struct (8 f32s), avoids lifetime gymnastics in test sites.
    pub need_signals: crate::combat::ai::appraisal::NeedSignals,
    /// Step 6.3: stored goal context for repair-affinity consumer in
    /// `finalize_scores`. `None` when the actor has no stored goal (first
    /// tick or after Cast/EndTurn). Reference borrowed from `AiMemory.last_goal`
    /// for the duration of `pick_action`.
    pub last_goal: Option<&'p crate::combat::ai::repair::StoredGoalContext>,
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
            need_signals: self.need_signals,
            last_goal: None, // perspective-shifted ctx is always a sub-step — no goal needed
        }
    }
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Top-level decision function. Replaces evaluate_targets + plan_movement.
/// When `debug` is true, returns an `AiDebugSnapshot` alongside the decision.
/// Always returns a `ChosenInfo` on the normal path (None only for the
/// no-plans fallback) so callers can store the winning plan in `AiMemory`.
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
) -> (AiDecision, Option<AiDebugSnapshot>, Option<ChosenInfo>) {
    let log_on = logger.is_enabled();
    let t0 = if log_on { Some(std::time::Instant::now()) } else { None };

    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None, None);
    };

    // Apply per-actor AiTuning override if present. The swap is local to
    // pick_action — every downstream call (intent, plans, ranking, ScoringCtx)
    // sees the per-actor tuning through `world.tuning` without API changes.
    // In current content no unit declares `ai_tuning_override`, so this branch
    // is inert; scaffolding is here to support quirks (see step 2.7 of
    // docs/ai_rework_plan.md).
    let per_actor_tuning = active
        .ai_tuning_override
        .as_ref()
        .map(|ov| world.tuning.apply_override(ov));
    let per_actor_world;
    let world: &AiWorld = if let Some(ref t) = per_actor_tuning {
        per_actor_world = AiWorld { tuning: t, ..*world };
        &per_actor_world
    } else {
        world
    };

    // Compute need signals once per actor; consumed by select_intent (step 3.2)
    // and downstream factors/sanity via ScoringCtx (steps 3.3–3.5).
    // Must run before select_intent so the intent gate reads fresh signals.
    let need_signals = crate::combat::ai::appraisal::compute_need_signals(
        active, snap, maps, memory, world.tuning,
    );

    // ── Select tactical intent ──────────────────────────────────────────
    let choice = select_intent(active, snap, maps, memory, world.difficulty, world.tuning, &need_signals);
    update_memory(memory, active, &choice.intent, world.tuning);

    // ── Generate plans (beam search over depths) ───────────────────────
    let mut plans = generate_plans(actor, world, snap, maps);

    if plans.is_empty() {
        let decision = fallback::fallback_move(active, snap, maps);
        let ds = if debug {
            Some(build_fallback_debug(
                active, actor_pos, &choice.intent, &choice.reason, &decision,
                "no plans generated", snap, world.tuning, maps, debug_names,
            ))
        } else { None };
        return (decision, ds, None);
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
        need_signals,
        last_goal: memory.last_goal.as_ref(),
    };

    // ── Phase pipeline ─────────────────────────────────────────────────
    // Step 7.1: ViabilityStage + SanityStage run through the typed pipeline.
    // `PlanRanking::initial` computes the initial scored/raw_factors; the
    // pool is seeded from those values so both representations agree.
    let initial_scored;
    let initial_raw;
    {
        // Scope limiting the &mut borrow of plans for initial scoring.
        let (s, r) = crate::combat::ai::planning::score_plans_with_raw(
            &mut plans, &choice.intent, &scoring_ctx,
        );
        initial_scored = s;
        initial_raw = r;
    }
    let mut pool = ScoredPool::new(plans);
    pool.scored = initial_scored;
    pool.raw_factors = initial_raw;

    let mut stage_ctx = StageCtx::new(
        &scoring_ctx,
        choice.intent,
        choice.reason,
        actor_pos,
        rng,
    );
    let scoring_pipeline = Pipeline::new()
        .add(Box::new(ViabilityStage))
        .add(Box::new(SanityStage));
    scoring_pipeline.run(&mut pool, &mut stage_ctx);

    // Snapshot post-sanity scores before adaptation rescores any plan.
    // The log stores both numbers per plan so offline diagnostics can tell
    // "did adaptation move this rank?" without rerunning the pipeline.
    let base_scored = pool.scored.clone();

    // Contract stages (7.2): stages carry internal predicates and self-skip
    // when their conditions don't apply, so no `if matches!` guards needed.
    // RepairAffinityStage (7.3): populates annotation.repair_affinity per plan.
    let contract_pipeline = Pipeline::new()
        .add(Box::new(AdaptationStage))
        .add(Box::new(ProtectSelfMaskStage))
        .add(Box::new(KillableGateStage))
        .add(Box::new(RepairAffinityStage));
    contract_pipeline.run(&mut pool, &mut stage_ctx);

    // Sync pool back into PlanRanking for the legacy pipeline tail (pick,
    // log, debug). pool.plans/annotations remain alive until the end of
    // pick_action; we pass pool by reference where possible.
    let mut ranking = PlanRanking::from_pool(&pool, stage_ctx.intent, stage_ctx.intent_reason);
    plans = pool.plans;

    let (best_idx, mech) = ranking.pick(world, rng);

    // Step 7.2: if adaptation switched the chosen plan's evaluation regime,
    // wrap the intent reason so logs/debug see the full chain
    // (prior_intent_reason → adapted_via X). Now reads from pool.annotations
    // written by AdaptationStage instead of ranking.adaptation.reasons.
    if let Some(adapt_data) = pool.annotations.get(best_idx).and_then(|a| a.adaptation.as_ref()) {
        let prior = std::mem::replace(&mut ranking.intent_reason, IntentReason::NoRuleDefault);
        ranking.intent_reason = IntentReason::Adapted {
            prior: Box::new(prior),
            reason: adapt_data.reason.clone(),
        };
    }
    let pick_mech = debug.then_some(mech);

    let best_plan = &plans[best_idx];
    let best_score = ranking.scored[best_idx];
    let (decision, consumed) = commit_plan(best_plan, actor_pos);

    // Debug formatter walks the plans directly — one `ScoredStep` per plan
    // representing the committed first-tick action.
    let debug_snapshot = if debug {
        Some(build_debug_snapshot(
            active, actor_pos, &ranking.intent, &ranking.intent_reason, &plans,
            &ranking.scored, &ranking.raw_factors, &decision, snap, world.tuning, maps,
            debug_names, pick_mech.as_ref(),
        ))
    } else {
        None
    };

    // Capture reservation state before this actor writes its own — the log
    // entry reflects what prior actors have reserved, not this actor's output.
    let reservations_snap = if log_on { reservations.to_snapshot() } else {
        crate::combat::ai::log::ReservationsSnapshot::default()
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
            &ranking.sanity_breakdown,
            ranking.gate_stats.applied, ranking.gate_stats.pruned_count,
            best_idx, &decision, debug_names,
            world.difficulty, memory, reservations_snap,
        );
    }

    // Build ChosenInfo for plan-freeze continuation. Clear sim_snapshots to
    // avoid carrying heavy simulation data across ticks.
    let mut chosen_plan = best_plan.clone();
    chosen_plan.sim_snapshots.clear();
    let chosen = Some(ChosenInfo {
        plan: chosen_plan,
        score: best_score,
        intent: ranking.intent,
        reason: ranking.intent_reason.clone(),
    });

    (decision, debug_snapshot, chosen)
}

/// Write one JSONL entry for the decision that `pick_action` just produced.
/// Kept out of `pick_action` so the hot path stays focused on decisioning;
/// the top-K sort and string formatting only run when logging is enabled.
#[allow(clippy::too_many_arguments)]
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
    sanity_breakdown: &[Vec<crate::combat::ai::planning::SanityHit>],
    gate_applied: bool,
    gate_pruned_count: usize,
    best_idx: usize,
    decision: &AiDecision,
    debug_names: &HashMap<Entity, String>,
    difficulty: &DifficultyProfile,
    memory: &AiMemory,
    reservations_snap: crate::combat::ai::log::ReservationsSnapshot,
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
            let plan_sanity = sanity_breakdown.get(idx).map(|v| v.as_slice()).unwrap_or(&[]);
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
                plan_sanity,
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
        gate_applied,
        gate_pruned_count,
        difficulty,
        memory,
        reservations_snap,
    );
    if let Err(e) = logger.write_entry(&entry) {
        warn!("AI log write failed: {}", e);
    }
}
