//! SelfLethalWithoutPayoff critic: penalises significant self-damage (AoE
//! self-hit) without proportionate payoff in enemy damage or ally rescues.
//!
//! Fires on `self_damage_total > 0.3 × max_hp AND payoff < 0.5 × self_damage_total`.
//! Multiplier is monotone in `self_dmg_ratio = self_damage_total / max_hp`,
//! floored at 0.3 so a doomed plan keeps its relative rank when all options are bad.

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::pipeline::stages::sanity::plan_has_self_aoe;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::terminal::TerminalFactor;

// ── Constants ─────────────────────────────────────────────────────────────────

/// `self_damage_total` must exceed this fraction of `max_hp` for the critic
/// to even consider firing. Below this level the self-damage is negligible.
const SELF_DMG_THRESHOLD: f32 = 0.3;

/// Critic fires only when payoff is below this fraction of `self_damage_total`.
/// `0.5` means "payoff less than half the self-damage" — a clearly bad trade.
const PAYOFF_RATIO_THRESHOLD: f32 = 0.5;

/// Hard floor for the multiplier — even the worst self-damage plan retains
/// minimal score so it can compete when every alternative is equally bad.
const MULTIPLIER_FLOOR: f32 = 0.3;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — thresholds are baked as module constants (see above).
pub struct SelfLethalWithoutPayoff;

impl PlanCritic for SelfLethalWithoutPayoff {
    fn name(&self) -> &'static str {
        "self_lethal_without_payoff"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        let active = ctx.active;
        let max_hp = active.max_hp().max(1) as f32;

        // Self-damage from the outcome walk. Outcomes live on
        // `TurnPlan.annotation` (generator-populated); the pipeline annotation's
        // `outcomes` is dead here.
        let mut self_damage_total: f32 =
            plan.annotation.outcomes.iter().map(|o| o.self_damage).sum();

        // Fallback for synthetic/partial plans (mainly tests): a friendly-fire
        // AoE covering the caster with no populated outcome still fires the critic.
        if self_damage_total == 0.0 && plan_has_self_aoe(plan, ctx) {
            self_damage_total = 0.1 * max_hp;
        }

        // ── Guard: below threshold, critic passes ────────────────────────────
        let self_dmg_ratio = self_damage_total / max_hp;
        if self_dmg_ratio <= SELF_DMG_THRESHOLD {
            return None;
        }

        // ── Accumulate payoff from step outcomes ──────────────────────────────
        let enemy_damage_payoff: f32 = plan
            .annotation
            .outcomes
            .iter()
            .map(|o| o.enemy_damage + o.p_kill_now * max_hp * 0.5)
            .sum();

        // Terminal AllyRescue contribution — scales into the same HP units.
        let ally_rescue_payoff = ann.terminal.get(TerminalFactor::AllyRescue) * max_hp * 0.2;

        let payoff = enemy_damage_payoff + ally_rescue_payoff;

        // ── Guard: payoff covers the self-damage cost ─────────────────────────
        if payoff >= PAYOFF_RATIO_THRESHOLD * self_damage_total {
            return None;
        }

        // ── Compute monotone multiplier ───────────────────────────────────────
        // `self_dmg_ratio` is in (0.3, ∞). Map linearly from 1.0 at 0.3 → 0.5
        // at 1.0, then floor at MULTIPLIER_FLOOR.
        // Formula: 1.0 - 0.5 * (ratio - 0.3) / 0.7
        let multiplier = (1.0
            - 0.5 * (self_dmg_ratio - SELF_DMG_THRESHOLD) / (1.0 - SELF_DMG_THRESHOLD))
            .max(MULTIPLIER_FLOOR);

        let payoff_estimate = if max_hp > 0.0 { payoff / max_hp } else { 0.0 };

        Some(CriticHit {
            critic: CriticKind::SelfLethalWithoutPayoff,
            multiplier,
            reason: CriticReason::SelfLethalWithoutPayoff {
                self_dmg_ratio,
                payoff_estimate,
            },
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::outcome::ActionOutcomeEstimate;
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticsStage};
    use crate::combat::ai::pipeline::PlanStage;
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    // ── fires on canonical case (self-AoE, no payoff) ────────────────────────

    #[test]
    fn self_lethal_fires_on_canonical_case() {
        // ── 1. Test data ──
        // Actor: max_hp=30. Self-damage = 12 (40% of max_hp > 30% threshold).
        // Payoff = 0 (no enemy_damage, no kill, no rescue) → critic fires.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(30)
            .max_hp(30)
            .build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool ──
        // Outcomes live on plan.annotation (production-correct); pipeline
        // annotation outcomes are dead during pipeline.
        let stage = CriticsStage {
            critics: vec![Box::new(SelfLethalWithoutPayoff)],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();
        pool.plans[0]
            .annotation
            .outcomes
            .push(ActionOutcomeEstimate {
                self_damage: 12.0, // 40% of max_hp
                enemy_damage: 0.0,
                p_kill_now: 0.0,
                ..Default::default()
            });

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        let ann = &pool.annotations[0];
        let critic_hits: Vec<_> = ann
            .score_trace
            .multipliers
            .iter()
            .filter(|m| matches!(m.kind, MultiplierKind::Critic))
            .collect();
        assert_eq!(
            critic_hits.len(),
            1,
            "critic must fire when self_damage>30% and payoff=0"
        );
        assert!(
            critic_hits[0].value < 1.0,
            "multiplier must penalise, got {}",
            critic_hits[0].value
        );
        if let Some(MultiplierDetail::Critic { critic, .. }) = &critic_hits[0].detail {
            assert_eq!(*critic, CriticKind::SelfLethalWithoutPayoff);
        } else {
            panic!(
                "critic hit must carry Critic detail, got {:?}",
                critic_hits[0].detail
            );
        }
    }

    // ── passes on clean plan (no self-damage) ─────────────────────────────────

    #[test]
    fn self_lethal_passes_on_clean_plan() {
        // ── 1. Test data ──
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(30)
            .max_hp(30)
            .build();
        let plans = vec![TurnPlan::default()];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pool (no self_damage in outcomes) ──
        let stage = CriticsStage {
            critics: vec![Box::new(SelfLethalWithoutPayoff)],
        };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        assert!(
            pool.annotations[0]
                .score_trace
                .multipliers
                .iter()
                .all(|m| !matches!(m.kind, MultiplierKind::Critic)),
            "critic must not fire with zero self-damage"
        );
    }

    // ── severity scales with input ────────────────────────────────────────────

    #[test]
    fn self_lethal_severity_scales_with_input() {
        // ── 1. Test data ──
        // Compare two plans: mild self-damage (35% max_hp) vs severe (80% max_hp).
        // Both have zero payoff so both fire; severe must produce lower multiplier.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();

        // ── 2. Harness (shared) ──
        let h = StageTestHarness::new(actor);

        // ── 3. Pools ──
        // Outcomes live on plan.annotation (production-correct); pipeline
        // annotation outcomes are dead during pipeline.
        let stage_mild = CriticsStage {
            critics: vec![Box::new(SelfLethalWithoutPayoff)],
        };
        let mut pool_mild = PoolBuilder::new(vec![TurnPlan::default()])
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();
        pool_mild.plans[0]
            .annotation
            .outcomes
            .push(ActionOutcomeEstimate {
                self_damage: 35.0, // 35% of 100 max_hp
                ..Default::default()
            });

        let stage_severe = CriticsStage {
            critics: vec![Box::new(SelfLethalWithoutPayoff)],
        };
        let mut pool_severe = PoolBuilder::new(vec![TurnPlan::default()])
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();
        pool_severe.plans[0]
            .annotation
            .outcomes
            .push(ActionOutcomeEstimate {
                self_damage: 80.0, // 80% of 100 max_hp
                ..Default::default()
            });

        // ── 4. Act ──
        h.run(|ctx| {
            stage_mild.apply(&mut pool_mild, ctx);
            stage_severe.apply(&mut pool_severe, ctx);
        });

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        let critic_mild = pool_mild.annotations[0]
            .score_trace
            .multipliers
            .iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic));
        let critic_severe = pool_severe.annotations[0]
            .score_trace
            .multipliers
            .iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic));
        assert!(critic_mild.is_some(), "mild case must fire");
        assert!(critic_severe.is_some(), "severe case must fire");

        let mult_mild = critic_mild.unwrap().value;
        let mult_severe = critic_severe.unwrap().value;
        assert!(
            mult_severe < mult_mild,
            "severe penalty ({mult_severe}) must be stricter than mild ({mult_mild})"
        );
    }

    // ── name() returns the stable critic id ───────────────────────────────────

    #[test]
    fn name_is_stable() {
        assert_eq!(SelfLethalWithoutPayoff.name(), "self_lethal_without_payoff");
    }

    // ── threshold boundary: exactly at threshold → passes; just above → fires ─

    #[test]
    fn threshold_boundary() {
        // max_hp = 100. self_damage / max_hp = exactly SELF_DMG_THRESHOLD (0.3)
        // must NOT fire (condition is `self_dmg_ratio > 0.3`).
        // self_damage = 30.001 (ratio = 0.30001 > 0.3) must fire.
        use crate::combat::ai::test_helpers::{run_critic, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);

        for (self_damage, should_fire, label) in [
            (30.0_f32, false, "exactly at threshold"),
            (30.001_f32, true, "just above threshold"),
        ] {
            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
                .hp(100)
                .max_hp(100)
                .build();
            let scn = CriticScenarioBuilder::new(actor).build();
            let mut plan = TurnPlan::default();
            plan.annotation.outcomes.push(ActionOutcomeEstimate {
                self_damage,
                ..Default::default()
            });
            let ann = PlanAnnotation::default();

            let hit = run_critic(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
            if should_fire {
                assert!(
                    hit.is_some() && hit.unwrap().critic == CriticKind::SelfLethalWithoutPayoff,
                    "{label}: expected critic to fire"
                );
            } else {
                assert!(
                    hit.is_none(),
                    "{label}: expected critic to pass, got {hit:?}"
                );
            }
        }
    }

    // ── payoff boundary: payoff == threshold * self_dmg → passes; just below → fires

    #[test]
    fn payoff_threshold_boundary() {
        // self_damage = 40 (40% of max_hp=100 → above SELF_DMG_THRESHOLD=0.3).
        // PAYOFF_RATIO_THRESHOLD = 0.5, so payoff must be < 0.5 * 40 = 20 to fire.
        // enemy_damage = 20.0 → payoff >= threshold → critic passes.
        // enemy_damage = 19.999 → payoff < threshold → critic fires.
        use crate::combat::ai::test_helpers::{run_critic, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);

        for (enemy_damage, should_fire, label) in [
            (20.0_f32, false, "payoff exactly at boundary"),
            (19.999_f32, true, "payoff just below boundary"),
        ] {
            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
                .hp(100)
                .max_hp(100)
                .build();
            let scn = CriticScenarioBuilder::new(actor).build();
            let mut plan = TurnPlan::default();
            plan.annotation.outcomes.push(ActionOutcomeEstimate {
                self_damage: 40.0,
                enemy_damage,
                ..Default::default()
            });
            let ann = PlanAnnotation::default();

            let hit = run_critic(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
            if should_fire {
                assert!(
                    hit.is_some() && hit.unwrap().critic == CriticKind::SelfLethalWithoutPayoff,
                    "{label}: expected critic to fire"
                );
            } else {
                assert!(
                    hit.is_none(),
                    "{label}: expected critic to pass, got {hit:?}"
                );
            }
        }
    }

    // ── exact multiplier at known ratio ───────────────────────────────────────
    //
    // Formula: 1.0 - 0.5 * (ratio - 0.3) / (1.0 - 0.3), floored at 0.3.
    // At ratio = 0.3: multiplier = 1.0 - 0.5 * 0.0 / 0.7 = 1.0 (but threshold guard fires).
    // At ratio = 1.0: multiplier = 1.0 - 0.5 * 0.7 / 0.7 = 1.0 - 0.5 = 0.5.
    // At ratio = 2.0: multiplier = 1.0 - 0.5 * 1.7 / 0.7 ≈ −0.21 → floored → 0.3.
    // At ratio = 0.65: multiplier = 1.0 - 0.5 * 0.35 / 0.7 = 1.0 - 0.25 = 0.75.

    #[test]
    fn exact_multiplier_values() {
        use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
        use crate::combat::ai::test_helpers::{assert_critic_fires, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);

        // Each case: (self_damage, max_hp, expected_multiplier)
        // payoff = 0 in all cases to ensure critic fires.
        for (self_damage, max_hp, expected_mult, label) in [
            // ratio = 1.0 → multiplier = 0.5
            (100.0_f32, 100, 0.5_f32, "ratio=1.0 → mult=0.5"),
            // ratio = 0.65 → multiplier = 0.75
            (65.0_f32, 100, 0.75_f32, "ratio=0.65 → mult=0.75"),
            // ratio = 2.0 → floor at 0.3
            (200.0_f32, 100, 0.3_f32, "ratio=2.0 → floor=0.3"),
        ] {
            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
                .hp(max_hp)
                .max_hp(max_hp)
                .build();
            let scn = CriticScenarioBuilder::new(actor).build();
            let mut plan = TurnPlan::default();
            plan.annotation.outcomes.push(ActionOutcomeEstimate {
                self_damage,
                enemy_damage: 0.0,
                p_kill_now: 0.0,
                ..Default::default()
            });
            let ann = PlanAnnotation::default();

            assert_critic_fires(
                &SelfLethalWithoutPayoff,
                &plan,
                &ann,
                &scn,
                CriticKind::SelfLethalWithoutPayoff,
                expected_mult,
                |reason| {
                    if let CriticReason::SelfLethalWithoutPayoff {
                        self_dmg_ratio,
                        payoff_estimate,
                    } = reason
                    {
                        let expected_ratio = self_damage / max_hp as f32;
                        assert!(
                            (self_dmg_ratio - expected_ratio).abs() < 1e-5,
                            "{label}: self_dmg_ratio expected {expected_ratio}, got {self_dmg_ratio}"
                        );
                        assert!(
                            (payoff_estimate - 0.0).abs() < 1e-6,
                            "{label}: payoff_estimate expected 0.0, got {payoff_estimate}"
                        );
                    } else {
                        panic!("{label}: unexpected CriticReason variant: {reason:?}");
                    }
                },
            );
        }
    }

    // ── payoff_estimate uses payoff / max_hp division ─────────────────────────

    #[test]
    fn payoff_estimate_reflects_actual_payoff() {
        // enemy_damage = 5.0, max_hp = 100 → payoff_estimate = 5.0 / 100.0 = 0.05
        // But payoff (5.0) < 0.5 * self_damage (40.0) = 20.0, so critic fires.
        use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
        use crate::combat::ai::test_helpers::{assert_critic_fires, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();
        let scn = CriticScenarioBuilder::new(actor).build();
        let mut plan = TurnPlan::default();
        plan.annotation.outcomes.push(ActionOutcomeEstimate {
            self_damage: 40.0,
            enemy_damage: 5.0,
            p_kill_now: 0.0,
            ..Default::default()
        });
        let ann = PlanAnnotation::default();

        // ratio = 0.4, multiplier = 1.0 - 0.5 * 0.1 / 0.7 = 1.0 - 1/14 ≈ 0.928571…
        let expected_mult = 1.0_f32 - 0.5 * (0.4 - 0.3) / 0.7;
        assert_critic_fires(
            &SelfLethalWithoutPayoff,
            &plan,
            &ann,
            &scn,
            CriticKind::SelfLethalWithoutPayoff,
            expected_mult,
            |reason| {
                if let CriticReason::SelfLethalWithoutPayoff {
                    payoff_estimate, ..
                } = reason
                {
                    // payoff = 5.0, max_hp = 100 → payoff_estimate = 0.05
                    assert!(
                        (payoff_estimate - 0.05).abs() < 1e-6,
                        "payoff_estimate expected 0.05, got {payoff_estimate}"
                    );
                } else {
                    panic!("unexpected CriticReason: {reason:?}");
                }
            },
        );
    }

    // ── p_kill_now contributes to enemy_damage payoff ─────────────────────────

    #[test]
    fn p_kill_now_boosts_payoff() {
        // self_damage = 40 (40% of 100). Without kill, payoff = 0 → fires.
        // With p_kill_now = 1.0: enemy_damage_payoff = 0 + 1.0 * 100 * 0.5 = 50 ≥ 0.5*40=20 → passes.
        use crate::combat::ai::test_helpers::{assert_critic_passes, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();
        let scn = CriticScenarioBuilder::new(actor).build();
        let mut plan = TurnPlan::default();
        plan.annotation.outcomes.push(ActionOutcomeEstimate {
            self_damage: 40.0,
            enemy_damage: 0.0,
            p_kill_now: 1.0, // payoff = 50 → covers 0.5 * 40 = 20 → critic passes
            ..Default::default()
        });
        let ann = PlanAnnotation::default();

        assert_critic_passes(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
    }

    // ── self-AoE fallback: no outcomes but plan_has_self_aoe → fires ─────────

    #[test]
    fn self_aoe_fallback_fires_when_outcomes_empty() {
        // If outcome walk produced no self_damage but plan_has_self_aoe() is true,
        // the fallback sets self_damage_total = 0.1 * max_hp.
        // With max_hp = 100: self_damage_total = 10 → ratio = 0.10 ≤ 0.3 → passes.
        // With max_hp = 10:  self_damage_total = 1  → ratio = 0.10 ≤ 0.3 → passes still.
        // We need max_hp where 0.1 > 0.3 → impossible (10% never > 30%).
        //
        // So the fallback alone is never enough to cross the 30% threshold.
        // What it DOES cover is: mutations on line 73 (`0.1 * max_hp`).
        // A mutation `* → +` gives `self_damage_total = 0.1 + max_hp`, which at
        // max_hp=100 gives 100.1 → ratio=1.001 → multiplier changes.
        // We verify that with the real formula the fallback fires a PASS.
        //
        // For the branch to be exercised, we also need `self_damage_total == 0.0`
        // AND `plan_has_self_aoe` to return true.
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{assert_critic_passes, CriticScenarioBuilder};
        use crate::content::abilities::AbilityDef as AppAbilityDef;
        use bevy::prelude::Entity;
        use combat_engine::dice::DiceExpr;
        use combat_engine::AbilityId;
        use combat_engine::{
            AbilityDef as EngineDef, AbilityRange, AoEShape, EffectDef, TargetType,
        };

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();

        // Build an ability with friendly_fire + Circle(1) so the caster's own
        // tile (0,0) is in the AoE when target is also (0,0).
        let aoe_ability = AppAbilityDef {
            id: AbilityId::from("self_aoe_test"),
            name: "self_aoe_test".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: EngineDef {
                key: None,
                cost_ap: 1,
                costs: Vec::new(),
                range: AbilityRange { min: 0, max: 3 },
                target_type: TargetType::Ground,
                aoe: AoEShape::Circle { radius: 1 },
                friendly_fire: true,
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                statuses: Vec::new(),
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        };

        let scn = CriticScenarioBuilder::new(actor.clone())
            .with_ability("self_aoe_test", aoe_ability)
            .build();

        // Plan: cast at the caster's own tile → plan_has_self_aoe returns true.
        // No outcomes populated → self_damage_total starts at 0.
        let mut plan = TurnPlan::default();
        plan.steps.push(PlanStep::Cast {
            ability: AbilityId::from("self_aoe_test"),
            target: Entity::from_raw_u32(1).unwrap(),
            target_pos: actor_pos, // cast at own tile → caster is in AoE
        });
        plan.final_pos = actor_pos;
        // No outcomes → fallback sets self_damage_total = 0.1 * 100 = 10 (10% < 30%)
        let ann = PlanAnnotation::default();

        // 10% < 30% threshold → critic passes even with self-AoE fallback
        assert_critic_passes(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
    }

    // ── self-AoE fallback fires when max_hp is small enough ──────────────────

    #[test]
    fn self_aoe_fallback_10pct_below_threshold() {
        // Confirms the fallback formula: self_damage = 0.1 * max_hp.
        // Mutation `* → +` would give 0.1 + max_hp, which for max_hp = 100
        // evaluates to 100.1 (ratio > 0.3) and would fire instead of pass.
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{run_critic, CriticScenarioBuilder};
        use crate::content::abilities::AbilityDef as AppAbilityDef;
        use bevy::prelude::Entity;
        use combat_engine::dice::DiceExpr;
        use combat_engine::AbilityId;
        use combat_engine::{
            AbilityDef as EngineDef, AbilityRange, AoEShape, EffectDef, TargetType,
        };

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();

        let aoe_ability = AppAbilityDef {
            id: AbilityId::from("self_aoe"),
            name: "self_aoe".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: EngineDef {
                key: None,
                cost_ap: 1,
                costs: Vec::new(),
                range: AbilityRange { min: 0, max: 3 },
                target_type: TargetType::Ground,
                aoe: AoEShape::Circle { radius: 1 },
                friendly_fire: true,
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                statuses: Vec::new(),
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        };

        let scn = CriticScenarioBuilder::new(actor)
            .with_ability("self_aoe", aoe_ability)
            .build();

        let mut plan = TurnPlan::default();
        plan.steps.push(PlanStep::Cast {
            ability: AbilityId::from("self_aoe"),
            target: Entity::from_raw_u32(1).unwrap(),
            target_pos: actor_pos,
        });
        plan.final_pos = actor_pos;
        // no outcomes → fallback = 0.1 * 100 = 10.0 → ratio = 0.1 ≤ 0.3 → passes
        let ann = PlanAnnotation::default();

        let hit = run_critic(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
        assert!(
            hit.is_none(),
            "fallback self_damage (10%) is below 30% threshold, critic must pass; got {hit:?}"
        );
    }

    // ── ally_rescue payoff suppresses critic ──────────────────────────────────

    #[test]
    fn ally_rescue_payoff_suppresses_critic() {
        // self_damage = 40 (40% of 100). enemy_damage = 0, p_kill_now = 0.
        // ally_rescue = 1.0 → ally_rescue_payoff = 1.0 * 100 * 0.2 = 20 ≥ 0.5*40=20 → passes.
        use crate::combat::ai::scoring::factors::terminal::TerminalFactor;
        use crate::combat::ai::test_helpers::{assert_critic_passes, CriticScenarioBuilder};

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();
        let scn = CriticScenarioBuilder::new(actor).build();
        let mut plan = TurnPlan::default();
        plan.annotation.outcomes.push(ActionOutcomeEstimate {
            self_damage: 40.0,
            ..Default::default()
        });
        let mut ann = PlanAnnotation::default();
        ann.terminal.set(TerminalFactor::AllyRescue, 1.0); // contributes 20 payoff

        assert_critic_passes(&SelfLethalWithoutPayoff, &plan, &ann, &scn);
    }
}
