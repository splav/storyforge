//! OvercommitIntoDanger critic — step 10.1.
//!
//! Fires when an actor moves into a dangerous path or provokes AoO while
//! already under HP pressure. Absorbs the logic of `SanityRule::Survival`
//! and `SanityRule::AoOBleed` from `sanity_adjust_plans`; those branches
//! are disabled in 10.1 and will be removed in 10.4.
//!
//! Combined penalty = **max** of both sources (not a product) so that the
//! two signals are independent — each represents a distinct hazard class
//! and their worst case is what matters.

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::scoring::horizon::expected_aoo_damage;
use crate::combat::ai::scoring::factors::aggregate::worst_path_danger;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::orchestration::ScoringCtx;

// ── Public types ──────────────────────────────────────────────────────────────

/// Which of the two hazard paths produced the larger penalty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OvercommitSource {
    /// Low-HP × worst path danger² signal (mirrors `SanityRule::Survival`).
    SurvivalPath,
    /// Expected AoO damage / actor HP (mirrors `SanityRule::AoOBleed`).
    AooBleed,
}

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — all configuration comes from `ctx.world.tuning.thresholds`.
pub struct OvercommitIntoDanger;

impl PlanCritic for OvercommitIntoDanger {
    fn name(&self) -> &'static str {
        "overcommit_into_danger"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        _ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        let active = ctx.active;
        let t = &ctx.world.tuning.thresholds;

        // ── Signal 1: Survival path ───────────────────────────────────────────
        // Low-HP actor crosses/rests on dangerous tiles.
        // Formula identical to SanityRule::Survival in sanity_adjust_plans.
        let max_path_danger = worst_path_danger(plan, ctx.maps);
        let hp_need = ((0.6 - active.hp_pct()) / 0.6).clamp(0.0, 1.0);
        let excess = (max_path_danger - 0.5).max(0.0);
        let surv = t.low_hp_factor * hp_need * excess * excess;
        let surv_multiplier = if surv > 0.0 {
            Some((1.0 - surv).max(t.survival_floor))
        } else {
            None
        };

        // ── Signal 2: AoO bleed ───────────────────────────────────────────────
        // Expected AoO damage from leaving adjacency with melee enemies that
        // have reactions remaining.
        // Formula identical to SanityRule::AoOBleed in sanity_adjust_plans.
        let enemies: Vec<_> = ctx.snap.enemies_of(active.team).collect();
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        let aoo_multiplier = if aoo_dmg > 0.0 {
            let ratio = (aoo_dmg / active.hp.max(1) as f32).min(1.0);
            Some((1.0 - t.aoo_penalty_k * ratio * ratio).max(t.aoo_risk_floor))
        } else {
            None
        };

        // ── Pick the stricter of the two ──────────────────────────────────────
        // A lower multiplier = stricter penalty. We choose the minimum multiplier
        // (worst case) and record which source it came from.
        let (multiplier, source) = match (surv_multiplier, aoo_multiplier) {
            (None, None) => return None,
            (Some(m), None) => (m, OvercommitSource::SurvivalPath),
            (None, Some(m)) => (m, OvercommitSource::AooBleed),
            (Some(sm), Some(am)) => {
                if sm <= am {
                    (sm, OvercommitSource::SurvivalPath)
                } else {
                    (am, OvercommitSource::AooBleed)
                }
            }
        };

        let ratio = match source {
            OvercommitSource::SurvivalPath => surv,
            OvercommitSource::AooBleed => (aoo_dmg / active.hp.max(1) as f32).min(1.0),
        };

        Some(CriticHit {
            critic: CriticKind::OvercommitIntoDanger,
            multiplier,
            reason: CriticReason::OvercommitIntoDanger { source, ratio },
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
    use crate::combat::ai::test_helpers::{
        StageTestHarness, UnitBuilder,
        assert_stage_critic_passes, assert_stage_critic_fires, run_critic, CriticScenarioBuilder,
    };
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn move_plan(path: Vec<crate::game::hex::Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            ..TurnPlan::default()
        }
    }

    // ── name is stable ────────────────────────────────────────────────────────

    // NOTE: overcommit name() mutations are NOT in the missed-mutant list;
    // this test is kept as a cheap safety net only.
    #[test]
    fn overcommit_name_is_stable() {
        let critic = OvercommitIntoDanger;
        assert_eq!(critic.name(), "overcommit_into_danger");
    }

    // ── fires on canonical case (low HP + high danger path) ───────────────────

    #[test]
    fn overcommit_fires_on_canonical_case() {
        // Actor: low HP (5/30, hp_pct ≈ 0.17) moves through a tile with
        // danger=0.9. Expected: critic fires with SurvivalPath source.
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos = hex_from_offset(3, 3);
        let dest_pos  = hex_from_offset(2, 3);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(5).max_hp(30).build();
        let mut h = StageTestHarness::new(actor);
        h.difficulty = DifficultyProfile::hard();
        h.maps.danger.add(dest_pos, 0.9);

        let stage = CriticsStage::single(OvercommitIntoDanger);
        let mut pool = PoolBuilder::new(vec![move_plan(vec![dest_pos])])
            .scores(&[1.0]).trace_base_eq_score().build();
        h.run(|ctx| stage.apply(&mut pool, ctx));

        let hit = pool.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("critic must fire for low-HP actor on high-danger path");
        assert!(hit.value < 1.0, "multiplier must be a penalty (< 1.0), got {}", hit.value);
        let Some(MultiplierDetail::Critic { critic, reason }) = &hit.detail else {
            panic!("critic hit must carry Critic detail, got {:?}", hit.detail);
        };
        assert_eq!(*critic, CriticKind::OvercommitIntoDanger);
        let CriticReason::OvercommitIntoDanger { source, .. } = reason else {
            panic!("expected OvercommitIntoDanger reason, got {reason:?}");
        };
        assert_eq!(*source, OvercommitSource::SurvivalPath);
    }

    // ── passes on clean plan (full HP, no danger) ─────────────────────────────

    #[test]
    fn overcommit_passes_on_clean_plan() {
        // Actor: full HP, moves to a safe tile.
        let actor_pos = hex_from_offset(0, 0);
        let dest_pos  = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(20).max_hp(20).build();
        let h = StageTestHarness::new(actor);

        assert_stage_critic_passes(&h, vec![move_plan(vec![dest_pos])], OvercommitIntoDanger);
    }

    // ── severity scales with input ────────────────────────────────────────────

    #[test]
    fn overcommit_severity_scales_with_input() {
        // Moderate (hp=12, danger=0.6) vs severe (hp=4, danger=0.9).
        // Severe must produce a strictly lower (more punishing) multiplier.
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos = hex_from_offset(3, 3);
        let dest_pos  = hex_from_offset(2, 3);
        let plan      = vec![move_plan(vec![dest_pos])];

        let mut h_mod = StageTestHarness::new(UnitBuilder::new(1, Team::Enemy, actor_pos).hp(12).max_hp(30).build());
        h_mod.difficulty = DifficultyProfile::hard();
        h_mod.maps.danger.add(dest_pos, 0.6);
        let stage_mod = CriticsStage::single(OvercommitIntoDanger);
        let mut pool_mod = PoolBuilder::new(plan.clone()).scores(&[1.0]).trace_base_eq_score().build();
        h_mod.run(|ctx| stage_mod.apply(&mut pool_mod, ctx));

        let mut h_sev = StageTestHarness::new(UnitBuilder::new(1, Team::Enemy, actor_pos).hp(4).max_hp(30).build());
        h_sev.difficulty = DifficultyProfile::hard();
        h_sev.maps.danger.add(dest_pos, 0.9);
        let stage_sev = CriticsStage::single(OvercommitIntoDanger);
        let mut pool_sev = PoolBuilder::new(plan).scores(&[1.0]).trace_base_eq_score().build();
        h_sev.run(|ctx| stage_sev.apply(&mut pool_sev, ctx));

        let mult_mod = pool_mod.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("moderate case must fire").value;
        let mult_sev = pool_sev.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("severe case must fire").value;
        assert!(
            mult_sev < mult_mod,
            "severe penalty ({mult_sev}) must be stricter than moderate ({mult_mod})",
        );
    }

    // ── survival-path multiplier is in a tight range (catches * → +/÷ mutations)

    /// With hp=6/max_hp=30, danger=0.9 and default thresholds (low_hp_factor=1.2,
    /// survival_floor=0.25):
    ///   hp_pct = 6/30 = 0.2
    ///   hp_need = (0.6 - 0.2) / 0.6 ≈ 0.667
    ///   excess  = 0.9 - 0.5 = 0.4
    ///   surv    = 1.2 * 0.667 * 0.4 * 0.4 ≈ 0.128
    ///   mult    = (1.0 - 0.128).max(0.25) ≈ 0.872
    ///
    /// Any * → + or * → / mutation on the `surv` line yields a substantially
    /// different result (e.g., 0.25 or 0.712), so [0.82, 0.92] is tight enough
    /// to kill all three operators while being robust to minor float differences.
    #[test]
    fn overcommit_survival_multiplier_tight_range() {
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos = hex_from_offset(3, 3);
        let dest_pos  = hex_from_offset(2, 3);

        // hp=6/30 → hp_pct=0.2; danger tile at dest=0.9.
        // Full HP (20/20) keeps hp_need≈0 so SurvivalPath stays negligible and
        // we isolate the survival signal with hp=6.
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(6).max_hp(30).build();
        let mut h = StageTestHarness::new(actor);
        // Use default difficulty (default thresholds: low_hp_factor=1.2,
        // survival_floor=0.25) so the formula is deterministic.
        h.maps.danger.add(dest_pos, 0.9);

        let stage = CriticsStage::single(OvercommitIntoDanger);
        let mut pool = PoolBuilder::new(vec![move_plan(vec![dest_pos])])
            .scores(&[1.0]).trace_base_eq_score().build();
        h.run(|ctx| stage.apply(&mut pool, ctx));

        let mult = pool.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("survival critic must fire")
            .value;

        // Expected ≈ 0.872 (see doc comment). [0.82, 0.92] kills * → + and * → / mutations.
        assert!(
            mult > 0.82 && mult < 0.92,
            "survival multiplier expected ≈ 0.872, got {mult}",
        );
    }

    // ── AoO-bleed path fires when actor leaves melee adjacency ────────────────

    #[test]
    fn overcommit_aoo_fires_when_leaving_adjacency() {
        // Actor at (3,3), enemy at (4,3) (adjacent). Plan moves actor away.
        // Enemy has reactions_left > 0 and aoo_expected_damage set.
        // With default thresholds and appropriate hp/aoo values the critic fires.
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos  = hex_from_offset(3, 3);
        let enemy_pos  = hex_from_offset(4, 3);
        let dest_pos   = hex_from_offset(2, 3); // move away from enemy

        // Actor: hp=10 so ratio = aoo_dmg/10 is non-trivial.
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(10).max_hp(20).build();
        // Enemy: adjacent, reactions remaining, expected AoO damage = 6.
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .aoo(6.0, 1)
            .build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];

        let stage = CriticsStage::single(OvercommitIntoDanger);
        let mut pool = PoolBuilder::new(vec![move_plan(vec![dest_pos])])
            .scores(&[1.0]).trace_base_eq_score().build();
        h.run(|ctx| stage.apply(&mut pool, ctx));

        let hit = pool.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("critic must fire when actor leaves adjacency with an AoO-capable enemy");
        assert!(hit.value < 1.0, "AoO multiplier must penalise, got {}", hit.value);
        let Some(MultiplierDetail::Critic { reason, .. }) = &hit.detail else {
            panic!("expected Critic detail");
        };
        let CriticReason::OvercommitIntoDanger { source, ratio } = reason else {
            panic!("expected OvercommitIntoDanger reason, got {reason:?}");
        };
        assert_eq!(*source, OvercommitSource::AooBleed, "source must be AooBleed");
        assert!(*ratio > 0.0, "ratio must be positive, got {ratio}");
    }

    // ── AoO-bleed: zero damage → does not fire ────────────────────────────────

    #[test]
    fn overcommit_aoo_does_not_fire_when_no_aoo_damage() {
        // Actor adjacent to an enemy WITHOUT reactions or aoo_expected_damage.
        // Critic must pass (AoO path: aoo_dmg=0 → no penalty).
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let dest_pos  = hex_from_offset(2, 3);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(10).max_hp(20).build();
        // Enemy has NO reactions and no aoo_expected_damage (defaults = 0).
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];

        assert_stage_critic_passes(&h, vec![move_plan(vec![dest_pos])], OvercommitIntoDanger);
    }

    // ── AoO multiplier is in a tight range (catches / → * / % and - → + / ÷)

    /// With actor hp=10, aoo_dmg=4 and default thresholds (aoo_penalty_k=2.0,
    /// aoo_risk_floor=0.25):
    ///   ratio = 4.0 / 10.0 = 0.4
    ///   mult  = (1.0 - 2.0 * 0.4 * 0.4).max(0.25) = (1.0 - 0.32).max(0.25) = 0.68
    ///
    /// Mutations on lines 70-71 (ratio division replaced by multiply/rem, or
    /// the arithmetic inside the multiplier formula altered) all yield values
    /// far outside [0.60, 0.76]:
    ///   / → * at ratio: ratio becomes 40 → clamped to 1.0 → mult = 0.25
    ///   - → + in mult:  mult = 1.0 + 0.32 = 1.32
    ///   * → + (penalty_k * ratio): 2+0.4=2.4; mult = 1.0 - 2.4*0.4 = 0.04 → floor 0.25
    #[test]
    fn overcommit_aoo_multiplier_tight_range() {
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let dest_pos  = hex_from_offset(2, 3);

        // hp=10, aoo_dmg=4 → ratio=0.4, expected multiplier ≈ 0.68
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(10).max_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .aoo(4.0, 1)
            .build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];
        // No danger on dest tile so only AoO fires, not SurvivalPath.
        // (Full HP → hp_need=0 → surv=0 → no survival signal.)

        let stage = CriticsStage::single(OvercommitIntoDanger);
        let mut pool = PoolBuilder::new(vec![move_plan(vec![dest_pos])])
            .scores(&[1.0]).trace_base_eq_score().build();
        h.run(|ctx| stage.apply(&mut pool, ctx));

        let mult = pool.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("AoO critic must fire")
            .value;

        // Expected ≈ 0.68 (see doc comment). Range is tight to kill mutations.
        assert!(
            mult > 0.60 && mult < 0.76,
            "AoO multiplier expected ≈ 0.68, got {mult}",
        );
    }

    // ── AoO ratio stored in reason is in expected range (covers line 94 / → *)

    #[test]
    fn overcommit_aoo_reason_ratio_correct() {
        // ratio = aoo_dmg / actor.hp, stored in CriticReason. When / is
        // replaced by * the ratio explodes, so checking it's in (0, 1] kills
        // that mutant.
        use crate::combat::ai::pipeline::stages::critics::CriticsStage;
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::test_helpers::PoolBuilder;

        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let dest_pos  = hex_from_offset(2, 3);

        // aoo_dmg=8, hp=20: ratio = 8/20 = 0.4, should be in (0.3, 0.5).
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).hp(20).max_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .aoo(8.0, 1)
            .build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];

        let stage = CriticsStage::single(OvercommitIntoDanger);
        let mut pool = PoolBuilder::new(vec![move_plan(vec![dest_pos])])
            .scores(&[1.0]).trace_base_eq_score().build();
        h.run(|ctx| stage.apply(&mut pool, ctx));

        let hit = pool.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic))
            .expect("AoO critic must fire");
        let Some(MultiplierDetail::Critic { reason, .. }) = &hit.detail else {
            panic!("expected Critic detail");
        };
        let CriticReason::OvercommitIntoDanger { source, ratio } = reason else {
            panic!("expected OvercommitIntoDanger reason");
        };
        assert_eq!(*source, OvercommitSource::AooBleed);
        // ratio = 8/20 = 0.4; mutations (/ → *) would give 8*20=160 → clamped 1.0.
        assert!(
            *ratio > 0.3 && *ratio < 0.5,
            "AoO ratio expected ≈ 0.4, got {ratio}",
        );
    }
}
