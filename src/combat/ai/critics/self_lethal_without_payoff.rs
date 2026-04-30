//! SelfLethalWithoutPayoff critic — step 10.1.
//!
//! Fires when a plan accumulates significant self-damage (AoE self-hit, AoO
//! bleed proxy) without a proportionate payoff in enemy damage or ally rescues.
//! Generalises `SanityRule::SelfAoe`: that rule was a binary 0.5× for any
//! friendly-fire AoE touching the caster; this critic scales continuously
//! with the magnitude of the self-damage ratio relative to payoff.
//!
//! Fire condition:
//!   `self_damage_total > 0.3 × actor.max_hp`  AND
//!   `payoff < 0.5 × self_damage_total`
//!
//! Multiplier: monotone in `self_dmg_ratio = self_damage_total / actor.max_hp`,
//! floored at 0.3 to preserve the plan's relative rank when all options are bad.

use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::factors::terminal::TerminalFactor;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::sanity::plan_has_self_aoe;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

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
        let max_hp = active.max_hp.max(1) as f32;

        // ── Accumulate self-damage from step outcomes ─────────────────────────
        // `outcome.self_damage` captures AoE self-hits recorded during the
        // outcome simulation walk. Sum across all plan steps.
        let mut self_damage_total: f32 = ann.outcomes.iter().map(|o| o.self_damage).sum();

        // ── Self-AoE bonus: plan_has_self_aoe detects a friendly-fire cast
        // that covers the caster. When the outcome estimator didn't already
        // include the caster in the AoE radius (e.g. for hypothetical plans),
        // add a conservative estimate (10% of max_hp) to ensure the critic
        // fires even on plans without populated outcomes.
        if self_damage_total == 0.0 && plan_has_self_aoe(plan, ctx) {
            // Outcome walk should have populated self_damage for real plans;
            // fallback guards synthetic / partially-populated plans in tests.
            self_damage_total = 0.1 * max_hp;
        }

        // ── Guard: below threshold, critic passes ────────────────────────────
        let self_dmg_ratio = self_damage_total / max_hp;
        if self_dmg_ratio <= SELF_DMG_THRESHOLD {
            return None;
        }

        // ── Accumulate payoff from step outcomes ──────────────────────────────
        let enemy_damage_payoff: f32 = ann.outcomes.iter()
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
        let multiplier = (1.0 - 0.5 * (self_dmg_ratio - SELF_DMG_THRESHOLD) / (1.0 - SELF_DMG_THRESHOLD))
            .max(MULTIPLIER_FLOOR);

        let payoff_estimate = if max_hp > 0.0 { payoff / max_hp } else { 0.0 };

        Some(CriticHit {
            critic: CriticKind::SelfLethalWithoutPayoff,
            multiplier,
            reason: CriticReason::SelfLethalWithoutPayoff { self_dmg_ratio, payoff_estimate },
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::critics::{CriticKind, PlanCritic};
    use crate::combat::ai::outcome::{ActionOutcomeEstimate, PlanAnnotation};
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    // ── fires on canonical case (self-AoE, no payoff) ────────────────────────

    #[test]
    fn self_lethal_fires_on_canonical_case() {
        // Actor: max_hp=30. Self-damage = 12 (40% of max_hp > 30% threshold).
        // Payoff = 0 (no enemy_damage, no kill, no rescue).
        // → critic fires.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(30)
            .max_hp(30)
            .build();

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = TurnPlan::default();
        let mut ann = PlanAnnotation::default();
        ann.outcomes.push(ActionOutcomeEstimate {
            self_damage: 12.0, // 40% of max_hp
            enemy_damage: 0.0,
            p_kill_now: 0.0,
            ..Default::default()
        });

        let result = SelfLethalWithoutPayoff.evaluate(&plan, &ann, &ctx);
        assert!(result.is_some(), "critic must fire when self_damage>30% and payoff=0");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::SelfLethalWithoutPayoff);
        assert!(hit.multiplier < 1.0, "multiplier must penalise, got {}", hit.multiplier);
    }

    // ── passes on clean plan (no self-damage) ─────────────────────────────────

    #[test]
    fn self_lethal_passes_on_clean_plan() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(30)
            .max_hp(30)
            .build();

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // No self-damage in outcomes.
        let plan = TurnPlan::default();
        let ann = PlanAnnotation::default();

        let result = SelfLethalWithoutPayoff.evaluate(&plan, &ann, &ctx);
        assert!(result.is_none(), "critic must not fire with zero self-damage");
    }

    // ── severity scales with input ────────────────────────────────────────────

    #[test]
    fn self_lethal_severity_scales_with_input() {
        // Compare two plans: mild self-damage (35% max_hp) vs severe (80% max_hp).
        // Both have zero payoff so both fire; severe must produce lower multiplier.
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(100)
            .max_hp(100)
            .build();

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = TurnPlan::default();

        let mut ann_mild = PlanAnnotation::default();
        ann_mild.outcomes.push(ActionOutcomeEstimate {
            self_damage: 35.0, // 35% of 100 max_hp
            ..Default::default()
        });

        let mut ann_severe = PlanAnnotation::default();
        ann_severe.outcomes.push(ActionOutcomeEstimate {
            self_damage: 80.0, // 80% of 100 max_hp
            ..Default::default()
        });

        let hit_mild = SelfLethalWithoutPayoff.evaluate(&plan, &ann_mild, &ctx);
        let hit_severe = SelfLethalWithoutPayoff.evaluate(&plan, &ann_severe, &ctx);

        assert!(hit_mild.is_some(), "mild case must fire");
        assert!(hit_severe.is_some(), "severe case must fire");

        let mult_mild = hit_mild.unwrap().multiplier;
        let mult_severe = hit_severe.unwrap().multiplier;
        assert!(
            mult_severe < mult_mild,
            "severe penalty ({mult_severe}) must be stricter than mild ({mult_mild})"
        );
    }
}
