//! Critics layer — holds the `PlanCritic` trait + associated types and the
//! `CriticsStage` dispatcher that runs them.
//!
//! Each critic evaluates a single plan after scoring and returns an
//! `Option<CriticHit>`:
//! - `None` = plan passes this critic (no action).
//! - `Some(hit)` = plan violates a heuristic; `hit.multiplier` is applied
//!   multiplicatively to `ann.score` by `CriticsStage`.

pub mod blindspot_ranged;
pub mod buff_into_void;
pub mod heal_without_rescue_value;
pub mod overcommit_into_danger;
pub mod rare_resource_for_low_impact;
pub mod self_lethal_without_payoff;

pub use blindspot_ranged::BlindspotRanged;
pub use buff_into_void::BuffIntoVoid;
pub use heal_without_rescue_value::HealWithoutRescueValue;
pub use overcommit_into_danger::{OvercommitIntoDanger, OvercommitSource};
pub use rare_resource_for_low_impact::RareResourceForLowImpact;
pub use self_lethal_without_payoff::SelfLethalWithoutPayoff;

use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::pipeline::effects::{
    apply_score_effect_stage, EffectObservation, EmittedEffect, ScoreEffectStage, ScoreHit,
};
use crate::combat::ai::pipeline::order::StageId;
use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::plan::types::TurnPlan;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A single heuristic check applied to one plan after base scoring.
///
/// Implementors must be `Send + Sync` so that `CriticsStage` can hold a
/// `Vec<Box<dyn PlanCritic>>` without extra constraints.
pub trait PlanCritic: Send + Sync {
    /// Short identifier used in logs and debug output (e.g. `"overcommit_into_danger"`).
    fn name(&self) -> &'static str;

    fn evaluate(&self, plan: &TurnPlan, ctx: &ScoringCtx) -> Option<CriticHit>;
}

// ── CriticKind ────────────────────────────────────────────────────────────────

/// Identifies which critic produced a `CriticHit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticKind {
    /// Unit is low-HP or has high AoO exposure and moves into danger.
    OvercommitIntoDanger,
    /// Self-damage AoE cast with negligible payoff (kills / ally rescues).
    SelfLethalWithoutPayoff,
    /// Ranged unit ends its turn without line-of-sight to any enemy.
    BlindspotRanged,
    /// Buff/status cast on an ally who already has the same buff active.
    BuffIntoVoid,
    /// Expensive mana-cost ability with low expected impact.
    RareResourceForLowImpact,
    /// Heal cast on an ally with high HP who is not in danger.
    HealWithoutRescueValue,
}

// ── CriticReason ──────────────────────────────────────────────────────────────

/// Structured context explaining why a critic fired.
///
/// Each variant corresponds to one concrete critic. New variants are added in
/// steps 10.1–10.3; `#[serde(tag = "kind")]` ensures forward-compatible JSON.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriticReason {
    /// `OvercommitIntoDanger` fired — records which hazard signal dominated.
    OvercommitIntoDanger {
        /// Which of the two input signals produced the stronger penalty.
        source: overcommit_into_danger::OvercommitSource,
        /// The normalised risk ratio used to derive the multiplier:
        /// `surv` for SurvivalPath, `aoo_dmg / actor.hp` for AooBleed.
        ratio: f32,
    },
    /// `SelfLethalWithoutPayoff` fired — records the damage and payoff ratios.
    SelfLethalWithoutPayoff {
        /// `self_damage_total / actor.max_hp`.
        self_dmg_ratio: f32,
        /// Normalised payoff estimate (`payoff / actor.max_hp`).
        payoff_estimate: f32,
    },
    /// `BlindspotRanged` fired — ranged actor ends turn with no visible enemies.
    BlindspotRanged {
        /// Number of enemies visible from `final_pos`. Always 0 when the critic
        /// fires; kept as a field for observability in structured logs.
        enemies_visible: u32,
    },
    /// `BuffIntoVoid` fired — status cast wasted on a target who already has
    /// the same effect active (or received it from an earlier step in the plan).
    BuffIntoVoid {
        /// ID of the ability whose buff was wasted.
        ability: String,
        /// `true` = target already had the status in the snapshot;
        /// `false` = the status was applied redundantly within the same plan.
        target_already_buffed: bool,
    },
    /// `RareResourceForLowImpact` fired — expensive mana ability dealt
    /// significantly less damage than expected.
    RareResourceForLowImpact {
        /// ID of the ability that consumed the resource.
        ability: String,
        /// Mana cost of the ability.
        cost: u8,
        /// `actual_enemy_damage / expected_damage` (clamped to [0, 1]).
        impact_ratio: f32,
    },
    /// `HealWithoutRescueValue` fired — heal cast on an ally with low rescue
    /// need (computed via the same `curves.rescue_ally` used by the appraisal
    /// layer for the `rescue_ally` need signal). Penalty scales continuously
    /// with `(1 - rescue_need)` instead of binary thresholds.
    HealWithoutRescueValue {
        /// Rescue-need score from `curves.rescue_ally.eval((1-hp_pct)*threat_proxy)`.
        /// Lower values trigger the critic; closer to 0 → harsher penalty.
        rescue_need: f32,
        /// Target's HP as a fraction of max HP. Kept for log readability.
        target_hp_pct: f32,
    },
}

// ── CriticHit ─────────────────────────────────────────────────────────────────

/// A single critic evaluation that fired for a plan.
///
/// `multiplier` is applied multiplicatively to `ann.score`
/// (`ann.score *= hit.multiplier`). Values < 1.0 penalise, values > 1.0 reward.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CriticHit {
    /// Which critic produced this hit.
    pub critic: CriticKind,
    /// Score multiplier to apply (< 1.0 = penalty, > 1.0 = bonus).
    pub multiplier: f32,
    /// Structured diagnostic context for this hit.
    pub reason: CriticReason,
}

// ── CriticsStage ──────────────────────────────────────────────────────────────

pub struct CriticsStage {
    critics: Vec<Box<dyn PlanCritic>>,
}

impl CriticsStage {
    /// Build the first-wave critic set: defensive (overcommit, self-lethal),
    /// positioning (blindspot), and resource/value clusters.
    pub fn first_wave() -> Self {
        Self {
            critics: vec![
                Box::new(OvercommitIntoDanger),
                Box::new(SelfLethalWithoutPayoff),
                Box::new(BlindspotRanged),
                Box::new(BuffIntoVoid),
                Box::new(RareResourceForLowImpact),
                Box::new(HealWithoutRescueValue),
            ],
        }
    }

    /// Construct a stage with a single critic. Used by test helpers to run
    /// one critic in isolation via the full stage flow.
    pub(crate) fn single(critic: impl PlanCritic + 'static) -> Self {
        Self {
            critics: vec![Box::new(critic)],
        }
    }
}

impl ScoreEffectStage for CriticsStage {
    fn id(&self) -> StageId {
        StageId::Critics
    }

    fn compute_effects(&self, ctx: &StageCtx, pool: &ScoredPool) -> Vec<EmittedEffect> {
        let mut emitted = Vec::new();
        for (plan_index, plan) in pool.plans.iter().enumerate() {
            for c in &self.critics {
                if let Some(hit) = c.evaluate(plan, ctx.scoring) {
                    emitted.push(EmittedEffect {
                        plan_index,
                        hit: ScoreHit::Multiplier(MultiplierHit {
                            kind: MultiplierKind::Critic,
                            value: hit.multiplier,
                            detail: None,
                        }),
                        observability: Some(EffectObservation::Critic(hit)),
                    });
                }
            }
        }
        emitted
    }
}

impl PlanStage for CriticsStage {
    fn name(&self) -> &'static str {
        "critics"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_score_effect_stage(self, pool, ctx);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::orchestration::ScoringCtx;
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, snapshot_from, PoolBuilder,
        StageTestHarness, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::DiceRng;

    // ── empty critics vec — no-op ──────────────────────────────────────────

    #[test]
    fn critics_stage_no_op_when_empty() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(), TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        // CriticsStage::first_wave() has no critics — scores and annotations
        // must be unchanged after apply().
        let stage = CriticsStage::first_wave();
        let mut pool = PoolBuilder::new(plans)
            .scores(&[0.8, 0.5])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        assert_eq!(
            pool.annotations[0].score, 0.8,
            "score must not change with empty critics"
        );
        assert_eq!(
            pool.annotations[1].score, 0.5,
            "score must not change with empty critics"
        );
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        for ann in &pool.annotations {
            assert!(
                ann.score_trace
                    .multipliers
                    .iter()
                    .all(|m| !matches!(m.kind, MultiplierKind::Critic)),
                "no critic multipliers expected for empty stage",
            );
        }
    }

    // ── mock critic fires and multiplies score ─────────────────────────────

    struct AlwaysHitCritic {
        multiplier: f32,
    }

    impl PlanCritic for AlwaysHitCritic {
        fn name(&self) -> &'static str {
            "always_hit"
        }

        fn evaluate(&self, _plan: &TurnPlan, _ctx: &ScoringCtx) -> Option<CriticHit> {
            use overcommit_into_danger::OvercommitSource;
            Some(CriticHit {
                critic: CriticKind::OvercommitIntoDanger,
                multiplier: self.multiplier,
                reason: CriticReason::OvercommitIntoDanger {
                    source: OvercommitSource::SurvivalPath,
                    ratio: 0.5,
                },
            })
        }
    }

    #[test]
    fn critics_stage_writes_hit_and_multiplies_score() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert — score halved, exactly one critic hit ──
        let ann = &pool.annotations[0];
        assert!(
            (ann.score - 0.5).abs() < 1e-6,
            "expected score 0.5 after 0.5× multiplier, got {}",
            ann.score,
        );
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        let critic_mults: Vec<_> = ann
            .score_trace
            .multipliers
            .iter()
            .filter(|m| matches!(m.kind, MultiplierKind::Critic))
            .collect();
        assert_eq!(
            critic_mults.len(),
            1,
            "expected exactly one critic multiplier in trace"
        );
        if let Some(MultiplierDetail::Critic { critic, .. }) = &critic_mults[0].detail {
            assert_eq!(*critic, CriticKind::OvercommitIntoDanger);
        } else {
            panic!(
                "critic multiplier must carry Critic detail, got {:?}",
                critic_mults[0].detail
            );
        }
    }

    // Regression: Critics run AFTER Finalize in the pipeline order
    // (ModeSelection → Finalize → Sanity → Critics → …), so a critic multiplier
    // is not wiped by Finalize rescoring from raw factors. Runs a partial
    // pipeline (Finalize → AlwaysHitCritic(0.5)) and asserts
    // final score == finalized_score × 0.5.

    #[allow(clippy::too_many_arguments)]
    fn run_partial_pipeline_with_critic(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        raw: Vec<crate::combat::ai::scoring::factors::PlanFactorValues>,
        adaptations: Vec<Option<crate::combat::ai::outcome::AdaptationData>>,
        actor: &crate::combat::ai::test_helpers::UnitFixture,
        snap: &crate::combat::ai::world::snapshot::BattleSnapshot,
        intent: TacticalIntent,
        critic_multiplier: f32,
    ) -> ScoredPool {
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = crate::combat::ai::world::reservations::Reservations::default();
        let scoring = make_scoring_ctx(&world, snap, &maps, &reservations, actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            intent,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        );

        let mut pool = PoolBuilder::new(plans)
            .scores(&scores)
            .factors(raw)
            .adaptations(adaptations)
            .build();

        // Run partial pipeline: ModeSelection already ran (adaptation is pre-injected),
        // so we start from FinalizeStage then Critics.
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        FinalizeStage.apply(&mut pool, &mut ctx);

        let score_after_finalize: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        // Apply mock critic.
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic {
                multiplier: critic_multiplier,
            })],
        };
        stage.apply(&mut pool, &mut ctx);

        // Verify: each score == finalized_score × multiplier.
        for (i, ann) in pool.annotations.iter().enumerate() {
            let expected = score_after_finalize[i] * critic_multiplier;
            assert!(
                (ann.score - expected).abs() < 1e-5,
                "plan[{i}]: critics multiplier was lost — expected {expected}, got {}",
                ann.score,
            );
        }

        pool
    }

    #[test]
    fn critics_survive_through_adaptation_path() {
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::scoring::factors::PlanFactorValues;

        // ── 1. Test data ──
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(10)
            .max_hp(20)
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);

        // Two plans: one with LastStand adaptation, one with Default (no adaptation).
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let scores = vec![0.8_f32, 0.6_f32];
        let raw = vec![PlanFactorValues::default(), PlanFactorValues::default()];
        let adaptations = vec![
            Some(AdaptationData {
                reason: AdaptationReason::ProtectSelfNoDefensive,
                original_score: 0.8,
                mode: crate::combat::ai::adapt::EvaluationMode::LastStand,
            }),
            None, // Default mode
        ];

        // Both plans must have critics multiplier applied after Finalize.
        // Assertions are inside run_partial_pipeline_with_critic.
        run_partial_pipeline_with_critic(
            plans,
            scores,
            raw,
            adaptations,
            &actor,
            &snap,
            TacticalIntent::Reposition,
            0.5,
        );
    }

    // ── P3a.2 — ScoreTrace integration tests ─────────────────────────────────

    /// Mock critic fires with multiplier 0.5; trace must contain exactly one
    /// MultiplierHit with kind=Critic and value=0.5 after apply().
    #[test]
    fn p3a_critics_push_multipliers_to_trace() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let trace = &pool.annotations[0].score_trace;
        assert_eq!(
            trace.multipliers.len(),
            1,
            "expected exactly one multiplier hit"
        );
        assert_eq!(trace.multipliers[0].kind, MultiplierKind::Critic);
        assert!(
            (trace.multipliers[0].value - 0.5).abs() < 1e-6,
            "multiplier value must be 0.5, got {}",
            trace.multipliers[0].value
        );
    }

    /// entry_score=1.0, multiplier=0.5: trace.base == 1.0 (initialised in setup,
    /// mirrors Finalize's role) and trace.compute() == 0.5 == ann.score.
    #[test]
    fn p3a_critics_trace_base_synced_from_score() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let stage = CriticsStage {
            critics: vec![Box::new(AlwaysHitCritic { multiplier: 0.5 })],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert!(
            (ann.score_trace.base - 1.0).abs() < 1e-6,
            "trace.base must be synced to entry score 1.0, got {}",
            ann.score_trace.base
        );
        let computed = ann.score_trace.compute();
        assert!(
            (computed - 0.5).abs() < 1e-6,
            "trace.compute() must equal 0.5, got {computed}"
        );
        assert!(
            (ann.score - computed).abs() < 1e-6,
            "ann.score must equal trace.compute(): {} vs {computed}",
            ann.score
        );
    }

    struct FixedMultiplierCritic {
        multiplier: f32,
    }

    impl PlanCritic for FixedMultiplierCritic {
        fn name(&self) -> &'static str {
            "fixed_multiplier"
        }
        fn evaluate(&self, _plan: &TurnPlan, _ctx: &ScoringCtx) -> Option<CriticHit> {
            use overcommit_into_danger::OvercommitSource;
            Some(CriticHit {
                critic: CriticKind::OvercommitIntoDanger,
                multiplier: self.multiplier,
                reason: CriticReason::OvercommitIntoDanger {
                    source: OvercommitSource::SurvivalPath,
                    ratio: 0.5,
                },
            })
        }
    }

    /// Two critics [0.5, 0.8]; entry=1.0: ann.score ≈ 0.4, trace.compute() ≈ 0.4,
    /// both multiplier hits present in push order.
    #[test]
    fn p3a_critics_invariant_score_equals_compute_with_multiple_hits() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;

        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let stage = CriticsStage {
            critics: vec![
                Box::new(FixedMultiplierCritic { multiplier: 0.5 }),
                Box::new(FixedMultiplierCritic { multiplier: 0.8 }),
            ],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        // 1.0 * 0.5 * 0.8 = 0.4
        assert!(
            (ann.score - 0.4).abs() < 1e-5,
            "expected score 0.4 after two multipliers, got {}",
            ann.score
        );
        let computed = ann.score_trace.compute();
        assert!(
            (computed - 0.4).abs() < 1e-5,
            "trace.compute() must equal 0.4, got {computed}"
        );
        assert_eq!(
            ann.score_trace.multipliers.len(),
            2,
            "expected 2 multiplier hits"
        );
        assert_eq!(ann.score_trace.multipliers[0].kind, MultiplierKind::Critic);
        assert!(
            (ann.score_trace.multipliers[0].value - 0.5).abs() < 1e-6,
            "first multiplier value must be 0.5"
        );
        assert!(
            (ann.score_trace.multipliers[1].value - 0.8).abs() < 1e-6,
            "second multiplier value must be 0.8"
        );
    }

    /// Empty critics list: trace.base == entry_score, multipliers empty,
    /// trace.compute() == entry_score.
    #[test]
    fn p3a_critics_no_hits_leave_trace_with_only_base() {
        // ── 1. Test data ──
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let entry_score = 0.75_f32;
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![] };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[entry_score])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        let ann = &pool.annotations[0];
        assert!(
            (ann.score_trace.base - entry_score).abs() < 1e-6,
            "trace.base must equal entry_score={entry_score}, got {}",
            ann.score_trace.base
        );
        assert!(
            ann.score_trace.multipliers.is_empty(),
            "multipliers must be empty when no critics fired"
        );
        assert!(
            (ann.score_trace.compute() - entry_score).abs() < 1e-6,
            "trace.compute() must equal entry_score={entry_score}"
        );
    }

    // ── Trait/types serde tests ────────────────────────────────────────────────

    #[test]
    fn plan_annotation_critics_default_empty() {
        use crate::combat::ai::outcome::PlanAnnotation;
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        let ann = PlanAnnotation::default();
        assert!(
            ann.score_trace
                .multipliers
                .iter()
                .all(|m| !matches!(m.kind, MultiplierKind::Critic)),
            "PlanAnnotation::default() must have no critic multipliers in trace",
        );
    }

    #[test]
    fn critic_kind_serde_round_trip() {
        // Sanity-check that all variants survive JSON round-trip (snake_case naming).
        let kinds = [
            CriticKind::OvercommitIntoDanger,
            CriticKind::SelfLethalWithoutPayoff,
            CriticKind::BlindspotRanged,
            CriticKind::BuffIntoVoid,
            CriticKind::RareResourceForLowImpact,
            CriticKind::HealWithoutRescueValue,
        ];
        for k in kinds {
            let json = serde_json::to_string(&k).expect("serialize");
            let back: CriticKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(k, back);
        }
    }

    #[test]
    fn critic_reason_serde_round_trip() {
        use overcommit_into_danger::OvercommitSource;
        let reasons: Vec<CriticReason> = vec![
            CriticReason::OvercommitIntoDanger {
                source: OvercommitSource::SurvivalPath,
                ratio: 0.5,
            },
            CriticReason::OvercommitIntoDanger {
                source: OvercommitSource::AooBleed,
                ratio: 0.8,
            },
            CriticReason::SelfLethalWithoutPayoff {
                self_dmg_ratio: 0.45,
                payoff_estimate: 0.1,
            },
            CriticReason::BlindspotRanged { enemies_visible: 0 },
            CriticReason::BuffIntoVoid {
                ability: "buff_shield".into(),
                target_already_buffed: true,
            },
            CriticReason::BuffIntoVoid {
                ability: "buff_shield".into(),
                target_already_buffed: false,
            },
            CriticReason::RareResourceForLowImpact {
                ability: "bolt".into(),
                cost: 40,
                impact_ratio: 0.15,
            },
            CriticReason::HealWithoutRescueValue {
                rescue_need: 0.05,
                target_hp_pct: 0.92,
            },
        ];
        for r in reasons {
            let json = serde_json::to_string(&r).expect("serialize");
            let back: CriticReason = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(r, back);
        }
    }

    #[test]
    fn critics_stage_name_is_stable() {
        let stage = CriticsStage::first_wave();
        assert_eq!(stage.name(), "critics");
    }
}
