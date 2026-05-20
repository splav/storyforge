//! Per-step factor enum and per-variant leaf modules.
//!
//! `StepFactor` covers 7 per-step axes: damage, kill_now, kill_promised, cc,
//! heal, scarcity, saturation. Array slots 0..7 in `PlanFactorValues`.
//!
//! Signed factors (can be negative): `Scarcity`, `Saturation`.

pub mod cc;
pub mod damage;
pub mod heal;
pub mod kill_now;
pub mod kill_promised;
pub mod saturation;
pub mod scarcity;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::scoring::factors::registry::{default_norm, BatchStats};
use crate::combat::ai::scoring::factors::ScoredStep;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::orchestration::ScoringCtx;

crate::factor_kind! {
    name: StepFactor,
    variants: [
        Damage,
        KillNow,
        KillPromised,
        Cc,
        Heal,
        Scarcity,
        Saturation,
    ]
}

impl StepFactor {
    /// String name used in serde named maps and `from_name`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Damage       => "damage",
            Self::KillNow      => "kill_now",
            Self::KillPromised => "kill_promised",
            Self::Cc           => "cc",
            Self::Heal         => "heal",
            Self::Scarcity     => "scarcity",
            Self::Saturation   => "saturation",
        }
    }

    /// True for factors that can be negative (use symmetric normalisation).
    pub fn signed(self) -> bool {
        matches!(self, Self::Scarcity | Self::Saturation)
    }

    /// Normalise `raw` against `batch` using this factor's sign policy.
    pub fn normalize(self, raw: f32, batch: &BatchStats) -> f32 {
        default_norm(raw, batch, self.signed())
    }

    /// Variant count (same as `COUNT`).
    pub fn count() -> usize { COUNT }

    /// Iterator over all variants in declaration order.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Damage,
            Self::KillNow,
            Self::KillPromised,
            Self::Cc,
            Self::Heal,
            Self::Scarcity,
            Self::Saturation,
        ]
        .into_iter()
    }

    /// Look up a variant by its string name. Returns `None` for unknown names.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "damage"        => Some(Self::Damage),
            "kill_now"      => Some(Self::KillNow),
            "kill_promised" => Some(Self::KillPromised),
            "cc"            => Some(Self::Cc),
            "heal"          => Some(Self::Heal),
            "scarcity"      => Some(Self::Scarcity),
            "saturation"    => Some(Self::Saturation),
            _ => None,
        }
    }

    /// Compute this factor for a single scored step.
    ///
    /// `ctx.snap` must be the **pre-step** snapshot (caller applies
    /// `ctx.with_perspective(&sim_actor, pre_snap)` before entering the step
    /// loop). `needs` is forwarded for future step-11 use; current bodies
    /// ignore it.
    pub fn compute(
        self,
        ctx: &ScoringCtx,
        step: &ScoredStep,
        outcome: &ActionOutcomeEstimate,
        needs: &NeedSignals,
    ) -> f32 {
        match self {
            Self::Damage       => damage::compute(ctx, step, outcome, needs),
            Self::KillNow      => kill_now::compute(ctx, step, outcome, needs),
            Self::KillPromised => kill_promised::compute(ctx, step, outcome, needs),
            Self::Cc           => cc::compute(ctx, step, outcome, needs),
            Self::Heal         => heal::compute(ctx, step, outcome, needs),
            Self::Scarcity     => scarcity::compute(ctx, step, outcome, needs),
            Self::Saturation   => saturation::compute(ctx, step, outcome, needs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::scoring::factors::compute_offensive_for_step;
    use crate::combat::ai::world::reservations::Reservations;
    
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::content::content_view::ContentView;
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Routing pin: each `StepFactor` variant must read its **own** column from
    /// `OffensiveFactors`. Catches wiring bugs (e.g. `Damage.compute` accidentally
    /// returning `.heal`) by building a fixture where every offensive column has a
    /// **different non-zero** value, then asserting each leaf returns the matching
    /// column.
    ///
    /// Behaviour of the underlying formulas (policy::damage::value, crit-fail
    /// adjustment, friendly fire, scarcity pricing, saturation penalty) is pinned
    /// in `factors::offensive::tests` and `factors::{scarcity,saturation}::tests`
    /// — this test only verifies dispatch/routing.
    #[test]
    fn step_factor_routes_each_variant_to_its_own_column() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);

        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, caster_pos).full_hp(100).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(50)
            .max_hp(100)
            .threat(20.0)
            .build();
        let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &actor);

        // Every offensive fact distinct so column mis-routing is detectable.
        let outcome = ActionOutcomeEstimate {
            enemy_damage: 30.0,
            p_kill_now: 0.7,
            p_kill_soon: 0.4,
            cc_turns_applied: 2.0,
            hp_restored: 15.0,
            ..Default::default()
        };

        let ability = AbilityId::from("melee_attack");
        let step = ScoredStep::Cast {
            ability: &ability,
            target: target.entity,
            target_pos,
            caster_tile: caster_pos,
        };
        let needs = NeedSignals::default();

        // Reference: pull all 5 columns through the shared core.
        let off = compute_offensive_for_step(&ctx, &step, &outcome);

        // Each leaf must return its own column — and only its own.
        assert_eq!(StepFactor::Damage.compute(&ctx, &step, &outcome, &needs), off.damage);
        assert_eq!(StepFactor::KillNow.compute(&ctx, &step, &outcome, &needs), off.kill_now);
        assert_eq!(StepFactor::KillPromised.compute(&ctx, &step, &outcome, &needs), off.kill_promised);
        assert_eq!(StepFactor::Cc.compute(&ctx, &step, &outcome, &needs), off.cc);
        assert_eq!(StepFactor::Heal.compute(&ctx, &step, &outcome, &needs), off.heal);

        // Discrimination check: at least three columns must differ between this
        // outcome (a damage+kill+heal+cc fixture). If they all coincide, the test
        // would silently mask wiring bugs — pin the divergence.
        let cols = [off.damage, off.kill_now, off.kill_promised, off.cc, off.heal];
        let unique: std::collections::HashSet<u32> = cols.iter().map(|v| v.to_bits()).collect();
        assert!(unique.len() >= 3, "fixture too symmetric to detect mis-routing: {cols:?}");
    }

    /// `Move` steps yield zero across every offensive variant — single
    /// behavioural pin, not duplicated per leaf.
    #[test]
    fn step_factor_move_step_yields_zero_offensive() {
        let content = ContentView::load_global_for_tests();
        let diff = DifficultyProfile::default();
        let world = make_test_ctx(&content, &diff);
        let tile = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, tile).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let res = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &res, &actor);

        let step = ScoredStep::Move { caster_tile: tile };
        let outcome = ActionOutcomeEstimate::default();
        let needs = NeedSignals::default();

        for f in StepFactor::iter() {
            assert_eq!(
                f.compute(&ctx, &step, &outcome, &needs), 0.0,
                "Move step must yield 0 for {}", f.name(),
            );
        }
    }
}
