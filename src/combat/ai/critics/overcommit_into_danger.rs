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

use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::sanity::expected_aoo_damage;
use crate::combat::ai::planning::scorer::worst_path_danger;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::utility::ScoringCtx;

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
    use crate::combat::ai::critics::PlanCritic;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn move_plan(path: Vec<crate::game::hex::Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // ── fires on canonical case (low HP + high danger path) ───────────────────

    #[test]
    fn overcommit_fires_on_canonical_case() {
        // Actor: low HP (5/30, hp_pct ≈ 0.17) moves through a tile with
        // danger=0.9. Expected: critic fires with SurvivalPath source.
        let actor_pos = hex_from_offset(3, 3);
        let dest_pos = hex_from_offset(2, 3);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(5)
            .max_hp(30)
            .build();

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(dest_pos, 0.9);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = move_plan(vec![dest_pos]);
        let ann = PlanAnnotation::default();
        let critic = OvercommitIntoDanger;

        let result = critic.evaluate(&plan, &ann, &ctx);
        assert!(result.is_some(), "critic must fire for low-HP actor on high-danger path");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::OvercommitIntoDanger);
        assert!(hit.multiplier < 1.0, "multiplier must be a penalty (< 1.0), got {}", hit.multiplier);
        if let CriticReason::OvercommitIntoDanger { source, .. } = hit.reason {
            assert_eq!(source, OvercommitSource::SurvivalPath);
        } else {
            panic!("expected OvercommitIntoDanger reason, got {:?}", hit.reason);
        }
    }

    // ── passes on clean plan (full HP, no danger) ─────────────────────────────

    #[test]
    fn overcommit_passes_on_clean_plan() {
        // Actor: full HP, moves to a safe tile, no nearby melee enemies.
        let actor_pos = hex_from_offset(0, 0);
        let dest_pos = hex_from_offset(1, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(20)
            .max_hp(20)
            .build();

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps(); // all danger = 0.0
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = move_plan(vec![dest_pos]);
        let ann = PlanAnnotation::default();
        let result = OvercommitIntoDanger.evaluate(&plan, &ann, &ctx);
        assert!(result.is_none(), "critic must not fire for full-HP actor on safe path");
    }

    // ── severity scales with input ────────────────────────────────────────────

    #[test]
    fn overcommit_severity_scales_with_input() {
        // Compares two setups: moderate (hp=12, danger=0.6) vs severe (hp=4, danger=0.9).
        // Severe must produce a strictly lower (more punishing) multiplier.
        let actor_pos = hex_from_offset(3, 3);
        let dest_pos = hex_from_offset(2, 3);

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let plan = move_plan(vec![dest_pos]);
        let ann = PlanAnnotation::default();

        // ── Moderate: hp=12/30 (hp_need≈0.33), danger=0.6 (excess=0.1) ─────
        let actor_mod = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(12).max_hp(30).build();
        let snap_mod = BattleSnapshot::new(vec![actor_mod.clone()], 1);
        let mut maps_mod = empty_maps();
        maps_mod.danger.add(dest_pos, 0.6);
        let ctx_mod = make_scoring_ctx(&world, &snap_mod, &maps_mod, &reservations, &actor_mod);
        let hit_mod = OvercommitIntoDanger.evaluate(&plan, &ann, &ctx_mod);

        // ── Severe: hp=4/30 (hp_need≈0.78), danger=0.9 (excess=0.4) ────────
        let actor_sev = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .hp(4).max_hp(30).build();
        let snap_sev = BattleSnapshot::new(vec![actor_sev.clone()], 1);
        let mut maps_sev = empty_maps();
        maps_sev.danger.add(dest_pos, 0.9);
        let ctx_sev = make_scoring_ctx(&world, &snap_sev, &maps_sev, &reservations, &actor_sev);
        let hit_sev = OvercommitIntoDanger.evaluate(&plan, &ann, &ctx_sev);

        assert!(hit_mod.is_some(), "moderate case must fire");
        assert!(hit_sev.is_some(), "severe case must fire");

        let mult_mod = hit_mod.unwrap().multiplier;
        let mult_sev = hit_sev.unwrap().multiplier;
        assert!(
            mult_sev < mult_mod,
            "severe penalty ({mult_sev}) must be stricter than moderate ({mult_mod})"
        );
    }
}
