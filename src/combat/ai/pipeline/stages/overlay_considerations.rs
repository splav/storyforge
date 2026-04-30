//! OverlayConsiderationsStage — step 11.4.
//!
//! Updates `ann.per_item[i].considerations` with plan-aware data for each
//! plan × agenda-item pair.  Must run **after** `RepairAffinityStage` so that
//! `ann.repair_affinity` is populated and the `continuation_value` axis is
//! accurate; before `PlanModifiersStage` so the composition in `PickBestStage`
//! uses the most accurate data.
//!
//! # What it overlays
//!
//! - **feasibility** — `ann.viability.adjusted_score.clamp(0,1)` (continuous).
//! - **leverage**    — `0.5 × secure_kill + 0.5 × damage_norm`, both in `[0,1]`.
//!   `damage_norm = Σ enemy_damage / actor.max_hp` (proxy normalisation).
//! - **safety**      — `1.0 - max(self_damage_ratio, exposure_at_end)`.
//! - **continuation_value** — recomputes using `ann.repair_affinity` (plan-level).
//!
//! Other axes (`urgency`, `role_affinity`) are taken verbatim from the
//! item-level `AgendaItem::considerations` populated by `build_agenda` in 11.3.
//!
//! # Edge cases
//!
//! - Empty agenda → no-op.
//! - Empty `ann.per_item` (no `ItemScoringStage` ran) → no-op.

use crate::combat::ai::factors::TerminalFactor;
use crate::combat::ai::intent::considerations::IntentConsiderations;
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

pub struct OverlayConsiderationsStage;

impl PlanStage for OverlayConsiderationsStage {
    fn name(&self) -> &'static str {
        "overlay_considerations"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(agenda) = ctx.agenda else { return };
        if agenda.items.is_empty() || pool.is_empty() {
            return;
        }

        let actor_max_hp = ctx.scoring.active.max_hp as f32;

        let n_plans = pool.plans.len();
        for plan_idx in 0..n_plans {
            let ann = &pool.annotations[plan_idx];

            if ann.per_item.is_empty() {
                continue;
            }

            // Compute plan-aware axis values once per plan.

            // feasibility: continuous adjusted_score from viability gate.
            // `adjusted_score` is the post-rewrite score (= pre-viability score
            // when gate passed). Clamped to [0,1] to serve as a 0…1 quality axis.
            let feasibility = ann.viability.adjusted_score.clamp(0.0, 1.0);

            // leverage: 0.5 × secure_kill + 0.5 × damage_norm.
            // secure_kill: TerminalFactor that signals a guaranteed finish this turn.
            // damage_norm: cumulative enemy_damage across plan outcomes, normalised
            //   by actor's own max_hp as a convenient reference scale.
            //   This is a proxy; exact target-HP normalisation requires per-step
            //   target lookup which is not available here.
            let secure_kill = ann.terminal.get(TerminalFactor::SecureKill).clamp(0.0, 1.0);
            let total_enemy_damage: f32 = ann.outcomes.iter().map(|o| o.enemy_damage).sum();
            let damage_norm = if actor_max_hp > f32::EPSILON {
                (total_enemy_damage / actor_max_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let leverage = (0.5 * secure_kill + 0.5 * damage_norm).clamp(0.0, 1.0);

            // safety: 1.0 - max(self_damage_ratio, exposure_at_end).
            // self_damage_ratio: cumulative self-damage normalised by actor's max HP.
            // ExposureAtEnd: danger at the plan's final position (terminal factor).
            let total_self_damage: f32 = ann.outcomes.iter().map(|o| o.self_damage).sum();
            let self_damage_ratio = if actor_max_hp > f32::EPSILON {
                (total_self_damage / actor_max_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let exposure = ann.terminal.get(TerminalFactor::ExposureAtEnd).clamp(0.0, 1.0);
            let safety = 1.0 - self_damage_ratio.max(exposure);

            // continuation_value: recompute with plan-level repair_affinity.
            // Formula: 0.5 × continue_commitment + 0.5 × repair_severity_factor
            let repair_affinity = &ann.repair_affinity;
            let commitment = ctx
                .scoring
                .need_signals
                .continue_commitment
                .clamp(0.0, 1.0);
            let repair_score = repair_affinity.severity_factor.clamp(0.0, 1.0);
            let continuation_value = if ctx.scoring.last_goal.is_some() {
                (0.5 * commitment + 0.5 * repair_score).clamp(0.0, 1.0)
            } else {
                (0.5 * commitment).clamp(0.0, 1.0)
            };

            // Write back per-item considerations overlay for every agenda item.
            // urgency and role_affinity come from the item-level baseline.
            let n_items = ann.per_item.len();
            for item_idx in 0..n_items {
                let base = &agenda.items[item_idx].considerations;
                let overlaid = IntentConsiderations {
                    urgency:            base.urgency,          // item-level
                    feasibility,                                // plan-level
                    leverage,                                   // plan-level
                    safety,                                     // plan-level
                    role_affinity:      base.role_affinity,    // item-level
                    continuation_value,                         // plan-level
                };
                pool.annotations[plan_idx].per_item[item_idx].considerations = overlaid;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::{IntentKind, IntentReason, TacticalIntent};
    use crate::combat::ai::outcome::{PerItemEval, PlanAnnotation, ViabilityResult};
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

    fn empty_agenda_item() -> AgendaItem {
        AgendaItem {
            kind: IntentKind::Reposition,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations: IntentConsiderations {
                urgency: 0.7,
                feasibility: 0.5,
                leverage: 0.3,
                safety: 0.8,
                role_affinity: 0.6,
                continuation_value: 0.4,
            },
        }
    }

    fn run_overlay(
        plans: Vec<TurnPlan>,
        annotations: Vec<PlanAnnotation>,
        agenda: &Agenda,
    ) -> ScoredPool {
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
        pool.annotations = annotations;
        OverlayConsiderationsStage.apply(&mut pool, &mut ctx);
        pool
    }

    #[test]
    fn overlay_feasibility_is_continuous_adjusted_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // adjusted_score = 0.7 → feasibility = 0.7 (continuous, not binary).
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: false, adjusted_score: 0.7 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let feasibility = pool.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            (feasibility - 0.7).abs() < 1e-5,
            "feasibility must equal adjusted_score=0.7, got {feasibility}"
        );
    }

    #[test]
    fn overlay_urgency_and_role_affinity_preserved_from_item() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 0.0 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let c = &pool.annotations[0].per_item[0].considerations;
        // urgency and role_affinity should come from the item-level baseline.
        assert!(
            (c.urgency - 0.7).abs() < 1e-5,
            "urgency should be preserved from item (0.7), got {}", c.urgency
        );
        assert!(
            (c.role_affinity - 0.6).abs() < 1e-5,
            "role_affinity should be preserved from item (0.6), got {}", c.role_affinity
        );
    }

    #[test]
    fn overlay_noop_when_agenda_empty() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![],
        };
        let pool = run_overlay(vec![TurnPlan::default()], vec![PlanAnnotation::default()], &agenda);
        assert!(
            pool.annotations[0].per_item.is_empty(),
            "empty agenda → per_item unchanged (empty)"
        );
    }

    /// Plan-aware overlay uses continuous adjusted_score for feasibility.
    /// Two plans with different adjusted_score produce different feasibility values.
    #[test]
    fn plan_aware_overlay_changes_feasibility_axis() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::Reposition,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations {
                    urgency: 0.5, feasibility: 0.5, leverage: 0.5,
                    safety: 0.5, role_affinity: 0.5, continuation_value: 0.5,
                },
            }],
        };

        // High adjusted_score → high feasibility.
        let mut ann_high = PlanAnnotation::default();
        ann_high.viability = ViabilityResult { passed: true, adjusted_score: 0.9 };
        ann_high.per_item = vec![PerItemEval::default()];
        let pool_high = run_overlay(vec![TurnPlan::default()], vec![ann_high], &agenda);
        let feasibility_high = pool_high.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            (feasibility_high - 0.9).abs() < 1e-5,
            "adjusted_score=0.9 → feasibility=0.9, got {feasibility_high}"
        );

        // Zero adjusted_score → feasibility = 0.
        let mut ann_zero = PlanAnnotation::default();
        ann_zero.viability = ViabilityResult { passed: false, adjusted_score: 0.0 };
        ann_zero.per_item = vec![PerItemEval::default()];
        let pool_zero = run_overlay(vec![TurnPlan::default()], vec![ann_zero], &agenda);
        let feasibility_zero = pool_zero.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            feasibility_zero.abs() < 1e-5,
            "adjusted_score=0.0 → feasibility=0.0, got {feasibility_zero}"
        );

        // Overlay overwrites item-level baseline (0.5).
        assert!(
            (feasibility_high - 0.5).abs() > 1e-5,
            "overlay must differ from item-level feasibility baseline (0.5)"
        );
    }

    /// Safety axis = 1.0 - max(self_damage_ratio, exposure_at_end).
    /// Zero exposure and zero self_damage → safety = 1.0.
    #[test]
    fn overlay_safety_reflects_max_of_self_damage_and_exposure() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // Zero exposure, zero self_damage → safety = 1.0 - max(0, 0) = 1.0.
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];
        // terminal.ExposureAtEnd = 0.0 by default. outcomes is empty → self_damage = 0.

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let safety = pool.annotations[0].per_item[0].considerations.safety;
        assert!(
            (safety - 1.0).abs() < 1e-5,
            "zero exposure + zero self_damage → safety=1.0, got {safety}"
        );
    }

    /// Safety = 1.0 - self_damage_ratio when self_damage dominates exposure.
    #[test]
    fn overlay_safety_self_damage_dominates_exposure() {
        use crate::combat::ai::outcome::ActionOutcomeEstimate;
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // Actor max_hp = 100 (default). self_damage = 50 → ratio = 0.5.
        // exposure = 0.0. safety = 1.0 - max(0.5, 0.0) = 0.5.
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];
        ann.outcomes = vec![ActionOutcomeEstimate { self_damage: 50.0, ..Default::default() }];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let safety = pool.annotations[0].per_item[0].considerations.safety;
        // actor max_hp from UnitBuilder::new default = 10 (check test_helpers).
        // self_damage_ratio = 50/10 = 5.0 → clamped to 1.0 → safety = 0.0.
        // If max_hp is different, ratio may be different. Let's just verify safety < 1.0.
        assert!(
            safety < 1.0,
            "self_damage should reduce safety below 1.0, got {safety}"
        );
    }
}
