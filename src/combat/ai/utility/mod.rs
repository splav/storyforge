//! Top-level AI decision pipeline.
//!
//! Layout:
//! - `fallback` — moves used when no plan candidates survive beam search.
//!
//! Scoring stages and plan selection live in `combat::ai::pipeline`.
//! Plan generation lives in `combat::ai::plan`.

#![allow(clippy::too_many_arguments)]

mod fallback;

pub use crate::combat::ai::pipeline::stages::pick_best::PickMechanics;

use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
use crate::content::content_view::ContentView;
use crate::combat::ai::log::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::config::tuning::AiTuning;
use crate::combat::ai::intent::{
    assign_band, AiMemory, IntentReason, TacticalIntent,
};
use crate::combat::ai::intent::agenda::Agenda;
use crate::combat::ai::intent::bands::{BandReason, PriorityBand};
use crate::combat::ai::log::{self, AiLogger, IntentBlock, TradeBlock};
use crate::combat::ai::pipeline::stages::pick_best::commit_plan;
use crate::combat::ai::plan::{
    generate_plans, TurnPlan,
};
use crate::combat::ai::world::reservations::Reservations;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::core::{AbilityId, DiceRng};
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::HashMap;


// ── Public types ────────────────────────────────────────────────────────────

#[derive(Clone)]
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

/// Result returned by `pick_action`. The orchestrator (`enemy_turn.rs`)
/// extracts what it needs: decision, debug snapshot, chosen plan info, and
/// the full pool for logging.
pub struct PickResult {
    /// The AI decision to execute this tick.
    pub decision: AiDecision,
    /// Index of the winning plan in `pool`.
    pub best_idx: usize,
    /// The scored pool with all annotations populated (including `chosen`/`pick`
    /// fields set by `PickBestStage`).
    pub pool: ScoredPool,
    /// Debug snapshot — `Some` when `debug=true`, `None` otherwise.
    pub debug_snapshot: Option<AiDebugSnapshot>,
    /// Intent selected for this turn (possibly updated by ViabilityStage).
    pub intent: TacticalIntent,
    /// Reason for intent selection (original select_intent reason, unmodified by adaptation).
    pub intent_reason: IntentReason,
    /// Adaptation reason that switched the chosen plan's evaluation regime,
    /// or `None` when the chosen plan was scored under `EvaluationMode::Default`.
    /// Parallel to `intent_reason` but carries the *adaptation* context — these
    /// two fields together replace the old `IntentReason::Adapted` wrapper.
    pub evaluation_mode_reason: Option<crate::combat::ai::adapt::AdaptationReason>,
    /// Pre-adaptation (post-sanity) scores — used by the log to show
    /// pre/post-adaptation deltas. Same length as `pool`.
    pub base_scored: Vec<f32>,
    /// Step 11.6: priority band and reason assigned this tick.
    pub band: (crate::combat::ai::intent::bands::PriorityBand, crate::combat::ai::intent::bands::BandReason),
    /// Step 11.6: agenda built this tick (items in raw_score-desc order).
    pub agenda: crate::combat::ai::intent::agenda::Agenda,
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
    /// Step 9.A: tag cache for effective_ai_tags writeback in pick_action.
    /// Default (empty) value is used in test contexts via `make_test_ctx`.
    pub ability_tags: &'a crate::combat::ai::world::tags::AbilityTagCache,
    /// Step 9.B commit 2: status tag cache for `compute_apply_cc` (HardCC filter).
    /// Default (empty) value is used in test contexts via `make_test_ctx`.
    pub status_tags: &'a crate::combat::ai::world::tags::StatusTagCache,
}

/// Bundle of every read-only context the scoring layer touches. Replaces
/// the 5-7 parameter signatures (active, ctx, snap, maps, reservations) that
/// used to thread through every factor / plan / picker function.
///
/// Two lifetime parameters because perspective `(active, snap)` can be swapped
/// mid-plan when scoring against a sim'd state — `with_perspective` returns
/// a fresh `ScoringCtx` reusing the world refs but with a shorter `'p`.
#[derive(Clone, Copy)]
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

/// Top-level decision function. Pure function: no side effects on `memory`,
/// `reservations`, or logging. The orchestrator (`enemy_turn.rs`) handles
/// those after receiving the `PickResult`.
///
/// Returns a `PickResult` with the decision, the annotated pool, and
/// diagnostics. On empty-plans path, `pool` is empty and `best_idx = 0`.
pub fn pick_action(
    actor: Entity,
    actor_pos: Hex,
    world: &AiWorld,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    rng: &mut DiceRng,
    memory: &AiMemory,
    reservations: &Reservations,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> PickResult {
    let Some(active) = snap.unit(actor) else {
        return PickResult {
            decision: AiDecision::EndTurn,
            best_idx: 0,
            pool: ScoredPool::empty(),
            debug_snapshot: None,
            intent: TacticalIntent::Reposition,
            intent_reason: IntentReason::NoRuleDefault,
            evaluation_mode_reason: None,
            base_scored: vec![],
            band: (PriorityBand::NormalTactical, BandReason::Normal),
            agenda: Agenda { band: PriorityBand::NormalTactical, items: vec![] },
        };
    };

    // Apply per-actor AiTuning override if present.
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

    // Compute need signals once per actor.
    let appraisal_ctx = crate::combat::ai::appraisal::AppraisalCtx {
        active,
        snap,
        maps,
        memory,
        tuning: world.tuning,
        ability_tags: world.ability_tags,
        status_tags: world.status_tags,
        content: world.content,
    };
    let need_signals = crate::combat::ai::appraisal::compute_need_signals(&appraisal_ctx);

    // ── Step 11.1: band assignment (computed for telemetry plumbing only) ──
    // Band is NOT used for routing here — routing lands in 11.4.
    // Explicit discard so reviewers can see the intent without compiler noise.
    let (band, band_reason) = assign_band(active, snap, maps, &need_signals, world.difficulty, world.tuning);

    // ── Step 11.2 / 11.4 / 11.5: agenda construction ─────────────────────
    // In 11.4, agenda is passed into StageCtx so ItemScoringStage and
    // PickBestStage can perform per-item composition.
    // In 11.5, `memory` is forwarded so NormalTactical band's stickiness
    // bonuses match prior `select_intent` behaviour.
    let agenda = crate::combat::ai::intent::build_agenda(
        band,
        &band_reason,
        active,
        snap,
        maps,
        &need_signals,
        world.difficulty,
        world.tuning,
        memory,
    );

    // ── Step 11.5: primary intent derived from agenda ─────────────────────
    // `choice` drives (a) the initial `score_plans_with_raw` pass (producing
    // `score_initial` used by PickBestStage's additive composition formula)
    // and (b) `StageCtx.intent` for stages that still read the legacy field.
    //
    // Primary item = agenda.items[0] (highest raw_score).  Fallback to
    // Reposition / NoRuleDefault when the agenda is unexpectedly empty —
    // this should not happen in practice because every band builder guarantees
    // at least one item, but we keep the fallback for robustness.
    let choice = if let Some(primary) = agenda.items.first() {
        crate::combat::ai::intent::IntentChoice {
            intent: primary.intent_for_scoring(),
            reason: primary.reason.clone(),
        }
    } else {
        crate::combat::ai::intent::IntentChoice {
            intent: TacticalIntent::Reposition,
            reason: IntentReason::NoRuleDefault,
        }
    };

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
        return PickResult {
            decision,
            best_idx: 0,
            pool: ScoredPool::empty(),
            debug_snapshot: ds,
            intent: choice.intent,
            intent_reason: choice.reason,
            evaluation_mode_reason: None,
            base_scored: vec![],
            band: (band, band_reason),
            agenda,
        };
    }

    // Bundle the read-only scoring inputs once.
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
    let (initial_scored, initial_raw) = {
        crate::combat::ai::plan::score_plans_with_raw(
            &mut plans, &choice.intent, &scoring_ctx,
        )
    };
    let mut pool = ScoredPool::new(plans);
    for (ann, (score, raw)) in pool.annotations.iter_mut().zip(initial_scored.into_iter().zip(initial_raw.into_iter())) {
        ann.score = score;
        ann.score_initial = score; // Step 11.4: snapshot pre-pipeline score for multiplier_ratio.
        ann.factors = raw;
    }

    // Step 9.A: populate effective_ai_tags per Cast step (diagnostic only).
    // Written here — after score/factors cycle — so it's available for future
    // consumers in 9.B without touching the scoring pipeline.
    for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
        ann.effective_ai_tags = plan
            .steps
            .iter()
            .filter_map(|step| match step {
                crate::combat::ai::plan::types::PlanStep::Cast { ability, .. } => {
                    Some(world.ability_tags.effective(ability))
                }
                _ => None,
            })
            .collect();
    }

    let mut stage_ctx = StageCtx::new(
        &scoring_ctx,
        choice.intent,
        choice.reason,
        actor_pos,
        rng,
    );

    // Step 11.4: attach agenda to stage context for per-item composition.
    if !agenda.items.is_empty() {
        stage_ctx = stage_ctx.with_agenda(&agenda);
    }

    use crate::combat::ai::pipeline::order::{
        run, PRODUCTION_PIPELINE_POST_MASK, PRODUCTION_PIPELINE_PRE_MASK,
    };

    run(PRODUCTION_PIPELINE_PRE_MASK, &mut pool, &mut stage_ctx);

    // Snapshot post-sanity/critics scores (after all multipliers applied,
    // before mask/gate stages).  Carried in PickResult.base_scored and used
    // by the decision log to show pre/post-adaptation deltas.
    let base_scored: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

    run(PRODUCTION_PIPELINE_POST_MASK, &mut pool, &mut stage_ctx);

    // Find winning plan index from PickBestStage annotation.
    let best_idx = pool.annotations.iter().position(|a| a.chosen).unwrap_or(0);

    // Compute the final intent/reason (possibly updated by ViabilityStage/ModeSelectionStage).
    let final_intent = stage_ctx.intent;
    let final_reason = stage_ctx.intent_reason;

    // If adaptation switched the chosen plan's evaluation regime, capture the
    // adaptation reason as a separate field (P7: replaces IntentReason::Adapted wrapper).
    let evaluation_mode_reason = pool
        .annotations
        .get(best_idx)
        .and_then(|a| a.adaptation.as_ref())
        .map(|adapt_data| adapt_data.reason.clone());

    let best_plan = &pool.plans[best_idx];
    let (decision, _consumed) = commit_plan(best_plan, actor_pos);

    // Build debug snapshot (reads scores/raw_factors from annotations).
    let scored: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
    let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
    let pick_mech = debug.then(|| {
        pool.annotations[best_idx]
            .pick
            .as_ref()
            .map(|p| p.mechanics.clone())
    }).flatten();
    let debug_snapshot = if debug {
        Some(build_debug_snapshot(
            active, actor_pos, &final_intent, &final_reason, &pool.plans,
            &scored, &raw_factors, &decision, snap, world.tuning, maps,
            debug_names, pick_mech.as_ref(),
        ))
    } else {
        None
    };

    PickResult {
        decision,
        best_idx,
        pool,
        debug_snapshot,
        intent: final_intent,
        intent_reason: final_reason,
        evaluation_mode_reason,
        base_scored,
        band: (band, band_reason),
        agenda,
    }
}

/// Write one JSONL decision log entry from a `PickResult`.
///
/// Reads scores, raw_factors, adaptation, and sanity data directly from
/// `result.pool.annotations` rather than from separate parallel vecs.
/// Called by the orchestrator (`enemy_turn.rs`) after `pick_action` returns.
pub fn write_decision_log_from_result(
    logger: &mut AiLogger,
    decision_time_ms: u64,
    actor: Entity,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    content: &ContentView,
    result: &PickResult,
    debug_names: &HashMap<Entity, String>,
    difficulty: &DifficultyProfile,
    memory: &AiMemory,
    reservations_snap: crate::combat::ai::log::ReservationsSnapshot,
) {
    use crate::combat::ai::adapt::EvaluationMode;

    let plan_id = logger.next_plan_id();
    let pool = &result.pool;
    let plans = &pool.plans;
    let best_idx = result.best_idx;

    let actor_value = crate::combat::ai::scoring::trade::unit_value(active, content);

    // Rank plans by final (adapted) score, keep top-10 for size budget.
    // Pre-compute evaluation modes (owned) so plan_to_log_entry borrows them
    // with the same lifetime as pool.annotations.
    let evaluation_modes: Vec<EvaluationMode> = pool
        .annotations
        .iter()
        .map(|ann| {
            if ann.adaptation.is_some() {
                EvaluationMode::LastStand
            } else {
                EvaluationMode::Default
            }
        })
        .collect();

    let mut indexed: Vec<(usize, f32)> = pool
        .annotations
        .iter()
        .enumerate()
        .map(|(i, a)| (i, a.score))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let shown = indexed.len().min(10);
    let plan_entries: Vec<_> = indexed
        .iter()
        .take(shown)
        .enumerate()
        .map(|(rank, &(idx, score))| {
            let ann = &pool.annotations[idx];
            let br = crate::combat::ai::scoring::trade::trade_delta(
                &plans[idx], active, snap, content,
            );
            let trade = TradeBlock {
                delta: br.delta,
                killed: br.killed_value,
                lost: br.lost_value,
                self_lost: br.self_lost,
                self_lethal: br.self_lethal,
                score: crate::combat::ai::scoring::trade::trade_score(&br, actor_value),
            };
            let adaptation_reason = ann.adaptation.as_ref().map(|d| &d.reason);
            let base_score = result.base_scored.get(idx).copied().unwrap_or(score);
            log::plan_to_log_entry(
                &plans[idx],
                rank + 1,
                idx == best_idx,
                &ann.factors,
                &ann.terminal,
                base_score,
                score,
                &evaluation_modes[idx],
                adaptation_reason,
                trade,
                &ann.sanity,
            )
        })
        .collect();

    let actor_name = debug_names
        .get(&actor)
        .map(|s| s.as_str())
        .unwrap_or("<unknown>");
    let reason_text = result.intent_reason.to_string();
    let intent_block = IntentBlock {
        intent: &result.intent,
        selection_kind: result.intent_reason.code(),
        reason_text: &reason_text,
        reason: &result.intent_reason,
        evaluation_mode_reason: result.evaluation_mode_reason.as_ref(),
    };

    // gate_stats: KillableGateStage writes contract annotations but doesn't
    // directly expose applied/pruned_count. Derive from annotations.
    let gate_applied = pool.annotations.iter().any(|a| {
        a.contract.as_ref().map(|c| c.mask == "killable_gate").unwrap_or(false)
    });
    let gate_pruned_count = pool.annotations.iter().filter(|a| {
        a.contract.as_ref().map(|c| c.mask == "killable_gate").unwrap_or(false)
    }).count();

    let entry = log::build_entry(
        plan_id, decision_time_ms, active, actor_name, snap, intent_block,
        plans.len(), shown, plan_entries, &result.decision,
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::world::tags::AbilityTag;
    use crate::combat::ai::world::tags::cache::build_caches;
    use crate::combat::ai::test_helpers::{empty_maps, UnitBuilder};
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::core::DiceRng;
    use std::collections::HashMap;

    /// Helper: run pick_action with a single actor against an enemy,
    /// return the annotations of all scored plans.
    fn run_pick(
        actor_abilities: &[&str],
        use_content_cache: bool,
    ) -> Vec<crate::combat::ai::outcome::PlanAnnotation> {
        let content = crate::content::content_view::ContentView::load_global_for_tests();
        let (status_tag_cache, ability_tag_cache) = build_caches(&content);
        let difficulty = DifficultyProfile::default();

        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(actor_abilities)
            .ap(2)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let actor_entity = actor.entity;
        let reservations = Reservations::default();

        let world = AiWorld {
            content: &content,
            difficulty: &difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: if use_content_cache { &ability_tag_cache }
                          else { crate::combat::ai::test_helpers::empty_ability_tag_cache() },
            status_tags: if use_content_cache { &status_tag_cache }
                         else { crate::combat::ai::test_helpers::empty_status_tag_cache() },
        };
        let memory = crate::combat::ai::intent::AiMemory::default();
        let mut rng = DiceRng::with_seed(0);

        let result = pick_action(
            actor_entity, actor_pos, &world, &snap, &maps, &mut rng,
            &memory, &reservations, false, &HashMap::new(),
        );
        result.pool.annotations
    }

    #[test]
    fn pick_action_populates_effective_ai_tags_per_cast_step() {
        // Actor with melee_attack; any plan that casts it should have OFFENSIVE in tags.
        let annotations = run_pick(&["melee_attack"], true);
        // At least one plan should exist.
        assert!(
            !annotations.is_empty(),
            "pick_action should produce at least one plan"
        );
        // Find any plan that has a Cast step (non-empty effective_ai_tags).
        let has_cast_plan = annotations
            .iter()
            .any(|ann| !ann.effective_ai_tags.is_empty());
        assert!(
            has_cast_plan,
            "at least one plan must have non-empty effective_ai_tags (cast melee_attack)"
        );
        // Every non-empty effective_ai_tags entry for melee_attack should have OFFENSIVE.
        for ann in &annotations {
            for tag_set in &ann.effective_ai_tags {
                assert!(
                    tag_set.contains_tag(AbilityTag::Offensive),
                    "melee_attack Cast step must have OFFENSIVE tag, got {:?}",
                    tag_set
                );
            }
        }
    }

    #[test]
    fn pick_action_move_only_plan_has_empty_effective_ai_tags() {
        // Actor with only `move` (ToggleMoveMode) — no offensive/rescue/etc abilities.
        // `move` classifies as empty(), so every entry in effective_ai_tags must be empty.
        let annotations = run_pick(&["move"], true);
        for ann in &annotations {
            for tag_set in &ann.effective_ai_tags {
                assert!(
                    tag_set.is_empty(),
                    "Plans with only 'move' ability must have all-empty AbilityTagSet entries, \
                     got {:?}", tag_set
                );
            }
        }
    }

    #[test]
    fn pick_action_override_propagates_to_annotation() {
        // Actor with melee_attack; override it to MOBILITY.
        // Plans with Cast(melee_attack) must show MOBILITY, not OFFENSIVE.
        let content_base = crate::content::content_view::ContentView::load_global_for_tests();
        let mut content = content_base.clone();
        let ability_id = crate::core::AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&ability_id) {
            def.ai_tags_override = Some(vec!["mobility".to_string()]);
        }
        let (status_tag_cache, ability_tag_cache) = build_caches(&content);

        let difficulty = DifficultyProfile::default();
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(&["melee_attack"])
            .ap(2)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let memory = crate::combat::ai::intent::AiMemory::default();
        let mut rng = DiceRng::with_seed(0);

        let world = AiWorld {
            content: &content,
            difficulty: &difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: &ability_tag_cache,
            status_tags: &status_tag_cache,
        };
        let result = pick_action(
            actor.entity, actor_pos, &world, &snap, &maps, &mut rng,
            &memory, &reservations, false, &HashMap::new(),
        );

        for ann in &result.pool.annotations {
            for tag_set in &ann.effective_ai_tags {
                assert!(
                    tag_set.contains_tag(AbilityTag::Mobility),
                    "override to MOBILITY must propagate to effective_ai_tags"
                );
                assert!(
                    !tag_set.contains_tag(AbilityTag::Offensive),
                    "OFFENSIVE must not appear when overridden to MOBILITY"
                );
            }
        }
    }
}
