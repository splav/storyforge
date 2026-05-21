//! BlindspotRanged critic — step 10.2.
//!
//! Fires when a ranged actor ends its turn in a position with no line-of-sight
//! to any living enemy. Porting `SanityRule::LosBlindspot` 1:1 from
//! `sanity_adjust_plans`; that branch is disabled in 10.2 and removed in 10.4.
//!
//! Fire condition:
//!   `actor.tags.contains(RANGED)` AND enemies list is non-empty AND
//!   no living enemy is visible from `plan.final_pos`.
//!
//! Multiplier: **0.3** (identical to the original sanity rule).

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::world::tags::AiTags;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::game::hex::has_los;
use std::collections::HashSet;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Multiplier applied when a ranged actor ends its turn blind to all enemies.
/// Matches the original `SanityRule::LosBlindspot` inline value.
const BLINDSPOT_MULTIPLIER: f32 = 0.3;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — all state comes from the `ScoringCtx` snapshot.
pub struct BlindspotRanged;

impl PlanCritic for BlindspotRanged {
    fn name(&self) -> &'static str {
        "blindspot_ranged"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        _ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        let active = ctx.active_view;

        // Gate: only applies to ranged units with at least one living enemy.
        if !active.cache.tags.contains(AiTags::RANGED) {
            return None;
        }

        let enemies: Vec<_> = ctx.snap.enemies_of(active.team).collect();
        if enemies.is_empty() {
            return None;
        }

        // LoS blockers: occupied tiles of LIVING units, excluding the actor's
        // final position and the target enemy itself (mirrors sanity rule).
        let occupied: HashSet<_> = ctx
            .snap
            .units
            .iter()
            .filter(|u| u.is_alive())
            .map(|u| u.pos)
            .collect();

        let final_pos = plan.final_pos;
        let can_see_any = enemies.iter().any(|e| {
            has_los(final_pos, e.pos, |mid| {
                occupied.contains(&mid) && mid != final_pos && mid != e.pos
            })
        });

        if can_see_any {
            return None;
        }

        Some(CriticHit {
            critic: CriticKind::BlindspotRanged,
            multiplier: BLINDSPOT_MULTIPLIER,
            reason: CriticReason::BlindspotRanged { enemies_visible: 0 },
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::stages::critics::CriticsStage;
    use crate::combat::ai::pipeline::PlanStage;
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::tags::AiTags;
    use crate::combat::ai::test_helpers::{PoolBuilder, StageTestHarness, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn move_plan(dest: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![dest] }],
            final_pos: dest,
            residual_ap: 1,
            ..TurnPlan::default()
        }
    }

    // ── fires on canonical case ───────────────────────────────────────────────

    #[test]
    fn blindspot_fires_on_canonical_case() {
        // RANGED actor ends at (0,0). Enemy at (4,0), blocked by an ally at (2,0).
        // The ally occupies the line between actor and enemy — no LoS.
        // ── 1. Test data ──
        let actor_pos = hex_from_offset(0, 0);
        let ally_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();

        let plans = vec![move_plan(actor_pos)]; // stays in place

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![ally, enemy];

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![Box::new(BlindspotRanged)] };
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
        assert_eq!(critic_hits.len(), 1, "critic must fire: RANGED with no LoS to enemy");
        let hit = critic_hits[0];
        assert!(
            (hit.value - BLINDSPOT_MULTIPLIER).abs() < 1e-6,
            "multiplier must be {BLINDSPOT_MULTIPLIER}, got {}", hit.value
        );
        if let Some(MultiplierDetail::Critic { critic, reason }) = &hit.detail {
            assert_eq!(*critic, CriticKind::BlindspotRanged);
            if let CriticReason::BlindspotRanged { enemies_visible } = reason {
                assert_eq!(*enemies_visible, 0);
            } else {
                panic!("expected BlindspotRanged reason, got {:?}", reason);
            }
        } else {
            panic!("critic multiplier must carry Critic detail, got {:?}", hit.detail);
        }
    }

    // ── passes on clean plan ──────────────────────────────────────────────────

    #[test]
    fn blindspot_passes_on_clean_plan() {
        // RANGED actor ends at (0,0). Enemy at (3,0) with no blocker in between.
        // ── 1. Test data ──
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(3, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        let plans = vec![move_plan(actor_pos)];

        // ── 2. Harness ──
        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![Box::new(BlindspotRanged)] };
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
            "critic must not fire: RANGED with clear LoS to enemy"
        );
    }

    // ── gate conditions as "scaling" contract ─────────────────────────────────

    #[test]
    fn blindspot_severity_scales_with_input() {
        // 1) Non-RANGED actor without LoS → no critic hit (gate: RANGED tag required).
        // 2) RANGED actor with no enemies → no critic hit (gate: enemies required).
        let actor_pos = hex_from_offset(0, 0);
        let ally_blocker_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);
        let plan = vec![move_plan(actor_pos)];

        // ── Case 1: melee actor (no RANGED tag), enemy blocked → no hit ─────
        // ── 1. Test data ──
        let melee_actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_blocker_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();

        // ── 2. Harness ──
        let mut h1 = StageTestHarness::new(melee_actor);
        h1.extra_units = vec![ally, enemy];

        // ── 3. Pool ──
        let stage = CriticsStage { critics: vec![Box::new(BlindspotRanged)] };
        let mut pool1 = PoolBuilder::new(plan.clone())
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h1.run(|ctx| stage.apply(&mut pool1, ctx));

        // ── 5. Assert ──
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        assert!(
            pool1.annotations[0].score_trace.multipliers.iter().all(|m| !matches!(m.kind, MultiplierKind::Critic)),
            "non-RANGED actor must not trigger blindspot critic"
        );

        // ── Case 2: RANGED actor with no enemies → no hit ─────────────────
        // ── 1. Test data ──
        let ranged_actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();

        // ── 2. Harness ──
        let h2 = StageTestHarness::new(ranged_actor); // no extra_units → no enemies

        // ── 3. Pool ──
        let stage2 = CriticsStage { critics: vec![Box::new(BlindspotRanged)] };
        let mut pool2 = PoolBuilder::new(plan)
            .scores(&[1.0])
            .trace_base_eq_score()
            .build();

        // ── 4. Act ──
        h2.run(|ctx| stage2.apply(&mut pool2, ctx));

        // ── 5. Assert ──
        assert!(
            pool2.annotations[0].score_trace.multipliers.iter().all(|m| !matches!(m.kind, MultiplierKind::Critic)),
            "RANGED actor with no enemies must not trigger blindspot critic"
        );
    }
}
