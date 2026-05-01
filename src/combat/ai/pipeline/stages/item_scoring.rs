//! ItemScoringStage — step 11.4.
//!
//! Computes per-agenda-item intent-dependent factors for every plan in the pool
//! **before** `ModeSelectionStage` modifies intent columns.  Results are stored
//! in `ann.per_item[i]` for later use by `PickBestStage`.
//!
//! # Pipeline position (step 11.4)
//!
//! ```text
//! Viability → ItemScoring → ModeSelection → Finalize → Sanity → Critics
//!           → ProtectSelfMask → KillableGate → RepairAffinity
//!           → OverlayConsiderations → PlanModifiers → PickBest
//! ```
//!
//! # What it does
//!
//! For each plan × agenda-item pair:
//!
//! 1. `intent_factor` — `compute_plan_intent_sum(plan, item.intent_for_scoring(), ctx)`.
//! 2. `tempo_factor`  — `compute_plan_tempo_gain(plan, item.intent_for_scoring(), ctx)`.
//! 3. `eligible`      — `true` if the plan is eligible under the item's intent.
//!    - `ProtectSelf` → `plan_is_defensive(ann.factors.get_plan(SelfSurvival), ε)`.
//!    - `FocusTarget` → `plan_is_offensive_vs(plan, target)` (primary path)
//!      OR ApproachTarget fallback (ForcedTargeting band only — see step 11.8
//!      Section A for full semantics and pool-level pre-pass guard).
//!    - All other kinds → `true` (no mask).
//!
//! # ApproachTarget eligibility (step 11.8 Section A)
//!
//! For `FocusTarget` items in the `ForcedTargeting` band, an additional eligibility
//! path activates **only** when no plan in the entire pool satisfies
//! `plan_is_offensive_vs(plan, taunter)`.  In that case, a plan is eligible if it
//! contains at least one `Move` step and its `final_pos` is strictly closer to
//! the taunter than `actor_start_pos` (hex distance, v1 geometric approximation).
//!
//! Pool-level pre-pass: computed once at the start of `apply()`, before the
//! per-plan loop.  The stage runs post-Viability per pipeline order, so the
//! pool reflects the post-Viability state: an unviable offensive plan cannot
//! block ApproachTarget eligibility for viable approach-only plans.
//!
//! # Edge cases
//!
//! - **Empty agenda**: stage is a no-op; `per_item` stays empty.
//! - **Empty pool**: early return.

use crate::combat::ai::scoring::factors::compute_plan_tempo_gain;
use crate::combat::ai::scoring::factors::PlanFactor;
use crate::combat::ai::intent::IntentKind;
use crate::combat::ai::intent::bands::PriorityBand;
use crate::combat::ai::outcome::{PerItemEval, RejectReason};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::pipeline::stages::sanity::plan_is_defensive;
use crate::combat::ai::pipeline::stages::killable_gate::plan_is_offensive_vs;
use crate::combat::ai::planning::compute_plan_intent_sum;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::game::hex::Hex;

pub struct ItemScoringStage;

/// Returns `true` if `plan` approaches `target_pos` from `actor_start_pos`.
///
/// Conditions (both must hold):
/// 1. The plan contains at least one `Move` step.
/// 2. `plan.final_pos` is strictly closer (hex distance) to `target_pos` than
///    `actor_start_pos` is.
///
/// Known v1 limitation: uses geometric hex distance, not path distance — a plan
/// that ends geometrically closer but on the other side of an obstacle still
/// counts as approach.  Path-distance refinement is deferred to backlog.
fn approaches_target(plan: &TurnPlan, actor_start_pos: Hex, target_pos: Hex) -> bool {
    let has_move = plan.steps.iter().any(|s| matches!(s, PlanStep::Move { .. }));
    if !has_move {
        return false;
    }
    plan.final_pos.unsigned_distance_to(target_pos)
        < actor_start_pos.unsigned_distance_to(target_pos)
}

impl PlanStage for ItemScoringStage {
    fn name(&self) -> &'static str {
        "item_scoring"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Edge case: empty agenda or empty pool → no-op.
        let Some(agenda) = ctx.agenda else { return };
        if agenda.items.is_empty() || pool.is_empty() {
            return;
        }

        let epsilon = ctx
            .scoring
            .world
            .tuning
            .thresholds
            .self_survival_epsilon;

        let n_plans = pool.plans.len();
        let n_items = agenda.items.len();

        // Pre-allocate per_item vecs to the correct length.
        for ann in pool.annotations.iter_mut() {
            ann.per_item = vec![PerItemEval::default(); n_items];
        }

        // ── Pool-level pre-pass for ApproachTarget eligibility (step 11.8 §A) ──
        //
        // Runs AFTER ViabilityStage (pipeline order guarantees this).  We scan
        // the post-Viability pool once: if ANY viable plan attacks the taunter
        // offensively, approach-only plans stay ineligible (pool-level fallback,
        // not per-plan fallback).
        //
        // Only meaningful for ForcedTargeting band; for other bands the flag is
        // false and the ApproachTarget branch in the per-plan loop is never taken.
        let pool_has_offensive_vs_taunter: bool = match agenda.band {
            PriorityBand::ForcedTargeting => {
                // Taunter = target of the first ForcedTargeting item.
                if let Some(taunter) = agenda.items.first().and_then(|item| item.target) {
                    pool.plans.iter().enumerate().any(|(idx, plan)| {
                        // Only count viable plans (post-Viability pool semantics: unviable
                        // plans are annotated but not removed; an unviable offensive plan must
                        // not block ApproachTarget eligibility for viable approach-only plans).
                        pool.annotations[idx].viability.passed
                            && plan_is_offensive_vs(plan, taunter)
                    })
                } else {
                    false
                }
            }
            _ => false, // ApproachTarget relaxation is ForcedTargeting-only.
        };

        // Actor's start-of-turn position: approach baseline.
        // `ctx.scoring.active.pos` is the snapshot taken at turn start (P4-verified
        // invariant: immutable during planning). See design doc Section A.
        let actor_start_pos = ctx.scoring.active.pos;

        for plan_idx in 0..n_plans {
            let plan = &pool.plans[plan_idx];
            let ann = &pool.annotations[plan_idx];

            // Snapshot the primary-intent raw factors for mask computation.
            // At this point `ann.factors` holds results of `score_plans_with_raw`
            // (primary-intent pass), which includes SelfSurvival.
            let self_survival = ann.factors.get_plan(PlanFactor::SelfSurvival);
            let viability_passed = ann.viability.passed;

            // Collect per-item evals without holding a borrow on pool.
            let evals: Vec<PerItemEval> = agenda
                .items
                .iter()
                .map(|item| {
                    let intent = item.intent_for_scoring();
                    let intent_factor =
                        compute_plan_intent_sum(plan, &intent, ctx.scoring);
                    let tempo_factor =
                        compute_plan_tempo_gain(plan, &intent, ctx.scoring);

                    let (eligible, reject_reason) = match item.kind {
                        IntentKind::ProtectSelf => {
                            if plan_is_defensive(self_survival, epsilon) {
                                (true, None)
                            } else {
                                (false, Some(RejectReason::NotDefensive))
                            }
                        }
                        IntentKind::FocusTarget => {
                            if let Some(target) = item.target {
                                if plan_is_offensive_vs(plan, target) {
                                    // Primary path: plan directly attacks the target.
                                    (true, None)
                                } else if agenda.band == PriorityBand::ForcedTargeting
                                    && !pool_has_offensive_vs_taunter
                                    && viability_passed
                                {
                                    // ApproachTarget fallback (step 11.8 §A):
                                    //   - ForcedTargeting band only (obligation, not guidance).
                                    //   - Pool has no viable offensive plan vs taunter.
                                    //   - Plan itself must be viable.
                                    //   - Plan must geometrically approach the taunter.
                                    // Taunter position from start-of-turn snapshot.
                                    let taunter_pos = ctx
                                        .scoring
                                        .snap
                                        .unit(target)
                                        .map(|u| u.pos);
                                    // Plan is in the ApproachTarget gate window; failure here
                                    // is specifically "did not approach", not "not offensive".
                                    // Use the dedicated reason for cleaner mining attribution.
                                    if let Some(tpos) = taunter_pos {
                                        if approaches_target(plan, actor_start_pos, tpos) {
                                            (true, None)
                                        } else {
                                            (false, Some(RejectReason::NotApproachingTarget))
                                        }
                                    } else {
                                        // Taunter no longer in snapshot — degenerate case;
                                        // can't approach a phantom target.
                                        (false, Some(RejectReason::NotApproachingTarget))
                                    }
                                } else {
                                    (false, Some(RejectReason::NotOffensiveVsTarget))
                                }
                            } else {
                                // No target assigned by build_agenda → no mask (eligible).
                                (true, None)
                            }
                        }
                        _ => (true, None),
                    };

                    PerItemEval {
                        intent_factor,
                        tempo_factor,
                        eligible,
                        reject_reason,
                        considerations: crate::combat::ai::intent::considerations::IntentConsiderations::default(),
                    }
                })
                .collect();

            pool.annotations[plan_idx].per_item = evals;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::outcome::{PlanAnnotation, ViabilityResult};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::core::DiceRng;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
    }

    /// A plan that moves toward `to`; final_pos = to.
    fn move_plan_to(to: Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![to] }],
            final_pos: to,
            ..TurnPlan::default()
        }
    }

    /// A plan that casts at `target_entity`.
    fn cast_plan_at(target: bevy::prelude::Entity, target_pos: Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "test".into(),
                target,
                target_pos,
            }],
            final_pos: target_pos,
            ..TurnPlan::default()
        }
    }

    fn agenda_with_items(band: PriorityBand, items: Vec<AgendaItem>) -> Agenda {
        Agenda { band, items }
    }

    fn agenda_item(kind: IntentKind) -> AgendaItem {
        AgendaItem {
            kind,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations: IntentConsiderations::default(),
        }
    }

    fn agenda_item_with_target(kind: IntentKind, target: bevy::prelude::Entity) -> AgendaItem {
        AgendaItem {
            kind,
            target: Some(target),
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations: IntentConsiderations::default(),
        }
    }

    /// Run stage with a given actor position, explicit snapshot, and annotations.
    fn run_stage_with_snap(
        plans: Vec<TurnPlan>,
        annotations: Vec<PlanAnnotation>,
        agenda: &Agenda,
        actor: UnitSnapshot,
        snap: BattleSnapshot,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        )
        .with_agenda(agenda);
        let mut pool = ScoredPool::new(plans);
        pool.annotations = annotations;
        ItemScoringStage.apply(&mut pool, &mut ctx);
        pool
    }

    fn run_stage(plans: Vec<TurnPlan>, agenda: &Agenda) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut annotations = vec![PlanAnnotation::default(); plans.len()];
        for ann in annotations.iter_mut() {
            ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        }
        run_stage_with_snap(plans, annotations, agenda, actor, snap)
    }

    #[test]
    fn item_scoring_empty_agenda_is_noop() {
        let agenda = agenda_with_items(PriorityBand::NormalTactical, vec![]);
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert!(
            pool.annotations[0].per_item.is_empty(),
            "empty agenda → per_item stays empty"
        );
    }

    #[test]
    fn item_scoring_populates_per_item_for_each_agenda_item() {
        let agenda = agenda_with_items(
            PriorityBand::NormalTactical,
            vec![
                agenda_item(IntentKind::Reposition),
                agenda_item(IntentKind::ProtectSelf),
            ],
        );
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert_eq!(
            pool.annotations[0].per_item.len(),
            2,
            "per_item length must equal agenda.items.len()"
        );
    }

    #[test]
    fn item_scoring_protect_self_ineligible_for_non_defensive_plan() {
        // A plan with SelfSurvival = 0 (default) is not defensive → eligible=false.
        let agenda = agenda_with_items(
            PriorityBand::CriticalSelfPreservation,
            vec![agenda_item(IntentKind::ProtectSelf)],
        );
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert!(
            !pool.annotations[0].per_item[0].eligible,
            "non-defensive plan under ProtectSelf must be ineligible"
        );
    }

    #[test]
    fn item_scoring_non_protect_self_item_is_eligible() {
        let agenda = agenda_with_items(
            PriorityBand::NormalTactical,
            vec![agenda_item(IntentKind::Reposition)],
        );
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert!(
            pool.annotations[0].per_item[0].eligible,
            "non-ProtectSelf/FocusTarget item must be eligible"
        );
    }

    /// FocusTarget item with a target but plan has no matching cast → ineligible.
    #[test]
    fn eligible_killable_gate_for_non_killable_focus_target() {
        // A default TurnPlan has no Cast steps → plan_is_offensive_vs returns false.
        // In ForcedTargeting band: ApproachTarget fallback requires a Move step AND
        // getting closer; empty_plan has neither, so stays ineligible.
        let target_ent = bevy::prelude::Entity::from_raw_u32(42).unwrap();
        let agenda = agenda_with_items(
            PriorityBand::ForcedTargeting,
            vec![AgendaItem {
                kind: IntentKind::FocusTarget,
                target: Some(target_ent),
                raw_score: 1.0,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        );
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert!(
            !pool.annotations[0].per_item[0].eligible,
            "non-offensive, non-approaching plan under FocusTarget must be ineligible"
        );
    }

    /// With no agenda in ctx (None), ItemScoringStage is a no-op.
    #[test]
    fn item_scoring_no_agenda_in_ctx_is_noop() {
        // Build ctx WITHOUT attaching agenda.
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        ); // No .with_agenda() call.

        let mut pool = ScoredPool::new(vec![empty_plan()]);
        ItemScoringStage.apply(&mut pool, &mut ctx);
        assert!(
            pool.annotations[0].per_item.is_empty(),
            "no agenda in ctx → per_item stays empty"
        );
    }

    /// LastStand mode doesn't break per-item scoring — ItemScoringStage runs
    /// before ModeSelection so it always uses the primary-intent raw factors.
    /// Smoke test: if plan has no adaptation, per_item is still populated.
    #[test]
    fn mode_laststand_does_not_break_per_item_scoring() {
        let agenda = agenda_with_items(
            PriorityBand::CriticalSelfPreservation,
            vec![
                agenda_item(IntentKind::ProtectSelf),
                agenda_item(IntentKind::Reposition),
            ],
        );
        // No adaptation annotation → mode = Default.
        let pool = run_stage(vec![empty_plan()], &agenda);
        assert_eq!(
            pool.annotations[0].per_item.len(),
            2,
            "per_item must be populated even without adaptation"
        );
        // Reposition item should be eligible.
        assert!(
            pool.annotations[0].per_item[1].eligible,
            "Reposition item must be eligible"
        );
    }

    // ── T5: ApproachTarget eligibility tests (step 11.8 §A) ──────────────────

    /// ForcedTargeting band + FocusTarget item + no offensive plan in pool +
    /// plan moves toward taunter → eligible via ApproachTarget fallback.
    #[test]
    fn approach_target_eligible_when_forced_and_no_offensive_in_pool() {
        // Actor at (0,0), taunter at (3,0). Move plan ends at (2,0) — closer.
        let actor_pos = hex_from_offset(0, 0);
        let taunter_pos = hex_from_offset(3, 0);
        let closer_pos = hex_from_offset(2, 0);
        let taunter_ent = bevy::prelude::Entity::from_raw_u32(99).unwrap();

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let taunter = UnitBuilder::new(99, Team::Player, taunter_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), taunter], 1);

        // One move-only plan: no offensive cast → pool_has_offensive_vs_taunter = false.
        let move_plan = move_plan_to(closer_pos);
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };

        let agenda = agenda_with_items(
            PriorityBand::ForcedTargeting,
            vec![agenda_item_with_target(IntentKind::FocusTarget, taunter_ent)],
        );

        let pool = run_stage_with_snap(
            vec![move_plan],
            vec![ann],
            &agenda,
            actor,
            snap,
        );

        assert!(
            pool.annotations[0].per_item[0].eligible,
            "approach-only plan in Forced band (no offensive in pool) must be eligible"
        );
    }

    /// ForcedTargeting + offensive plan present in pool → approach-only plan NOT eligible.
    #[test]
    fn approach_target_ineligible_when_forced_but_offensive_plan_in_pool() {
        let actor_pos = hex_from_offset(0, 0);
        let taunter_pos = hex_from_offset(3, 0);
        let closer_pos = hex_from_offset(2, 0);
        let taunter_ent = bevy::prelude::Entity::from_raw_u32(99).unwrap();

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let taunter = UnitBuilder::new(99, Team::Player, taunter_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), taunter], 1);

        // Plan 0: offensive cast at taunter → pool_has_offensive_vs_taunter = true.
        let cast_plan = cast_plan_at(taunter_ent, taunter_pos);
        // Plan 1: move-only toward taunter.
        let move_plan = move_plan_to(closer_pos);

        let mut ann_cast = PlanAnnotation::default();
        ann_cast.viability = ViabilityResult { passed: true, adjusted_score: 1.5 };
        let mut ann_move = PlanAnnotation::default();
        ann_move.viability = ViabilityResult { passed: true, adjusted_score: 0.8 };

        let agenda = agenda_with_items(
            PriorityBand::ForcedTargeting,
            vec![agenda_item_with_target(IntentKind::FocusTarget, taunter_ent)],
        );

        let pool = run_stage_with_snap(
            vec![cast_plan, move_plan],
            vec![ann_cast, ann_move],
            &agenda,
            actor,
            snap,
        );

        assert!(
            pool.annotations[0].per_item[0].eligible,
            "offensive plan must be eligible via primary path"
        );
        assert!(
            !pool.annotations[1].per_item[0].eligible,
            "approach-only plan must be ineligible when pool has an offensive plan"
        );
    }

    /// NormalTactical band → ApproachTarget relaxation does NOT apply.
    #[test]
    fn approach_target_ineligible_in_normal_tactical_band() {
        let actor_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(3, 0);
        let closer_pos = hex_from_offset(2, 0);
        let target_ent = bevy::prelude::Entity::from_raw_u32(99).unwrap();

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let target_unit = UnitBuilder::new(99, Team::Player, target_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target_unit], 1);

        let move_plan = move_plan_to(closer_pos);
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };

        // NormalTactical band: ApproachTarget must NOT apply.
        let agenda = agenda_with_items(
            PriorityBand::NormalTactical,
            vec![agenda_item_with_target(IntentKind::FocusTarget, target_ent)],
        );

        let pool = run_stage_with_snap(
            vec![move_plan],
            vec![ann],
            &agenda,
            actor,
            snap,
        );

        assert!(
            !pool.annotations[0].per_item[0].eligible,
            "approach-only plan in NormalTactical band must be ineligible (approach relaxation is Forced-only)"
        );
    }

    /// Forced + no offensive in pool + plan moves AWAY from taunter →
    /// approach gate is active but the specific guard fails. Reject reason
    /// must be `NotApproachingTarget`, not the generic `NotOffensiveVsTarget`.
    /// Pins the dedicated reason for clean mining attribution.
    #[test]
    fn approach_target_failure_uses_not_approaching_target_reason() {
        let actor_pos = hex_from_offset(2, 0);
        let taunter_pos = hex_from_offset(3, 0);
        // Plan ends FARTHER from taunter than actor's start.
        let farther_pos = hex_from_offset(0, 0);
        let taunter_ent = bevy::prelude::Entity::from_raw_u32(99).unwrap();

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let taunter = UnitBuilder::new(99, Team::Player, taunter_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), taunter], 1);

        // Move-only plan that moves AWAY from taunter (no approach).
        let move_away = move_plan_to(farther_pos);
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };

        // ForcedTargeting, no offensive plan in pool → approach gate active.
        let agenda = agenda_with_items(
            PriorityBand::ForcedTargeting,
            vec![agenda_item_with_target(IntentKind::FocusTarget, taunter_ent)],
        );

        let pool = run_stage_with_snap(
            vec![move_away],
            vec![ann],
            &agenda,
            actor,
            snap,
        );

        let per_item = &pool.annotations[0].per_item[0];
        assert!(
            !per_item.eligible,
            "plan that moves away from taunter must be ineligible"
        );
        assert_eq!(
            per_item.reject_reason,
            Some(RejectReason::NotApproachingTarget),
            "approach gate failure must set NotApproachingTarget, not NotOffensiveVsTarget"
        );
    }
}
