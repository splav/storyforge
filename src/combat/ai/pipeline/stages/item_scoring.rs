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
//!    - `FocusTarget` → `plan_is_offensive_vs(plan, target)`.
//!    - All other kinds → `true` (no mask).
//!
//! # Edge cases
//!
//! - **Empty agenda**: stage is a no-op; `per_item` stays empty.
//! - **Empty pool**: early return.

use crate::combat::ai::factors::compute_plan_tempo_gain;
use crate::combat::ai::factors::PlanFactor;
use crate::combat::ai::intent::IntentKind;
use crate::combat::ai::outcome::PerItemEval;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::{
    compute_plan_intent_sum, plan_is_defensive, plan_is_offensive_vs,
};

pub struct ItemScoringStage;

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

        for plan_idx in 0..n_plans {
            let plan = &pool.plans[plan_idx];
            let ann = &pool.annotations[plan_idx];

            // Snapshot the primary-intent raw factors for mask computation.
            // At this point `ann.factors` holds results of `score_plans_with_raw`
            // (primary-intent pass), which includes SelfSurvival.
            let self_survival = ann.factors.get_plan(PlanFactor::SelfSurvival);

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

                    let eligible = match item.kind {
                        IntentKind::ProtectSelf => plan_is_defensive(self_survival, epsilon),
                        IntentKind::FocusTarget => {
                            if let Some(target) = item.target {
                                plan_is_offensive_vs(plan, target)
                            } else {
                                true // no target → no mask
                            }
                        }
                        _ => true,
                    };

                    PerItemEval {
                        intent_factor,
                        tempo_factor,
                        eligible,
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
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::core::DiceRng;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn empty_plan() -> TurnPlan {
        TurnPlan::default()
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

    fn run_stage(plans: Vec<TurnPlan>, agenda: &Agenda) -> ScoredPool {
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
        )
        .with_agenda(agenda);
        let mut pool = ScoredPool::new(plans);
        ItemScoringStage.apply(&mut pool, &mut ctx);
        pool
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
        // A default TurnPlan has no Cast steps → plan_is_offensive_vs returns false → ineligible.
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
            "non-offensive plan under FocusTarget must be ineligible"
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
}
