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
    use crate::combat::ai::pipeline::stages::critics::CriticsStage;
    use crate::combat::ai::pipeline::PlanStage;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
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

    // ── fires on canonical case (low HP + high danger path) ───────────────────

    #[test]
    fn overcommit_fires_on_canonical_case() {
        // ── 1. Test data ──
        // Actor: low HP (5/30, hp_pct ≈ 0.17) moves through a tile with
        // danger=0.9. Expected: critic fires with SurvivalPath source.
        let actor_pos = hex_from_offset(3, 3);
        let dest_pos = hex_from_offset(2, 3);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(5)
            .max_hp(30)
            .build();
        let plans = vec![move_plan(vec![dest_pos])];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.difficulty = DifficultyProfile::hard();
        h.maps.danger.add(dest_pos, 0.9);

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![Box::new(OvercommitIntoDanger)] };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
        let ann = &pool.annotations[0];
        let critic_hits: Vec<_> = ann.score_trace.multipliers.iter()
            .filter(|m| matches!(m.kind, MultiplierKind::Critic))
            .collect();
        assert_eq!(critic_hits.len(), 1, "critic must fire for low-HP actor on high-danger path");
        let hit = critic_hits[0];
        assert!(hit.value < 1.0, "multiplier must be a penalty (< 1.0), got {}", hit.value);
        if let Some(MultiplierDetail::Critic { critic, reason }) = &hit.detail {
            assert_eq!(*critic, CriticKind::OvercommitIntoDanger);
            if let CriticReason::OvercommitIntoDanger { source, .. } = reason {
                assert_eq!(*source, OvercommitSource::SurvivalPath);
            } else {
                panic!("expected OvercommitIntoDanger reason, got {:?}", reason);
            }
        } else {
            panic!("critic hit must carry Critic detail, got {:?}", hit.detail);
        }
    }

    // ── passes on clean plan (full HP, no danger) ─────────────────────────────

    #[test]
    fn overcommit_passes_on_clean_plan() {
        // ── 1. Test data ──
        // Actor: full HP, moves to a safe tile, no nearby melee enemies.
        let actor_pos = hex_from_offset(0, 0);
        let dest_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(20)
            .max_hp(20)
            .build();
        let plans = vec![move_plan(vec![dest_pos])];

        // ── 2. Harness ──
        let h = StageTestHarness::new(actor); // all danger = 0.0, default difficulty

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![Box::new(OvercommitIntoDanger)] };
        let mut pool = PoolBuilder::new(plans)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h.run(|ctx| stage.apply(&mut pool, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        assert!(
            pool.annotations[0].score_trace.multipliers.iter().all(|m| !matches!(m.kind, MultiplierKind::Critic)),
            "critic must not fire for full-HP actor on safe path"
        );
    }

    // ── severity scales with input ────────────────────────────────────────────

    #[test]
    fn overcommit_severity_scales_with_input() {
        // ── 1. Test data ──
        // Compares two setups: moderate (hp=12, danger=0.6) vs severe (hp=4, danger=0.9).
        // Severe must produce a strictly lower (more punishing) multiplier.
        let actor_pos = hex_from_offset(3, 3);
        let dest_pos = hex_from_offset(2, 3);
        let plan = vec![move_plan(vec![dest_pos])];

        // ── Moderate: hp=12/30 (hp_need≈0.33), danger=0.6 (excess=0.1) ──────
        // ── 2. Harness (moderate) ──
        let actor_mod = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(12).max_hp(30).build();
        let mut h_mod = StageTestHarness::new(actor_mod);
        h_mod.difficulty = DifficultyProfile::hard();
        h_mod.maps.danger.add(dest_pos, 0.6);

        // ── 3. Pool (moderate) ──
        let stage_mod = CriticsStage { critics: vec![Box::new(OvercommitIntoDanger)] };
        let mut pool_mod = PoolBuilder::new(plan.clone())
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act (moderate) ──
        h_mod.run(|ctx| stage_mod.apply(&mut pool_mod, ctx));

        // ── Severe: hp=4/30 (hp_need≈0.78), danger=0.9 (excess=0.4) ─────────
        // ── 2. Harness (severe) ──
        let actor_sev = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(4).max_hp(30).build();
        let mut h_sev = StageTestHarness::new(actor_sev);
        h_sev.difficulty = DifficultyProfile::hard();
        h_sev.maps.danger.add(dest_pos, 0.9);

        // ── 3. Pool (severe) ──
        let stage_sev = CriticsStage { critics: vec![Box::new(OvercommitIntoDanger)] };
        let mut pool_sev = PoolBuilder::new(plan)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act (severe) ──
        h_sev.run(|ctx| stage_sev.apply(&mut pool_sev, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        let critic_mult_mod = pool_mod.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic));
        let critic_mult_sev = pool_sev.annotations[0].score_trace.multipliers.iter()
            .find(|m| matches!(m.kind, MultiplierKind::Critic));
        assert!(critic_mult_mod.is_some(), "moderate case must fire");
        assert!(critic_mult_sev.is_some(), "severe case must fire");

        let mult_mod = critic_mult_mod.unwrap().value;
        let mult_sev = critic_mult_sev.unwrap().value;
        assert!(
            mult_sev < mult_mod,
            "severe penalty ({mult_sev}) must be stricter than moderate ({mult_mod})"
        );
    }
}
