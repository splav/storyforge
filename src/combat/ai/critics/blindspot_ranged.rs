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

use crate::combat::ai::critics::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::snapshot::AiTags;
use crate::combat::ai::utility::ScoringCtx;
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
        let active = ctx.active;

        // Gate: only applies to ranged units with at least one living enemy.
        if !active.tags.contains(AiTags::RANGED) {
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
    use crate::combat::ai::critics::PlanCritic;
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot};
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn move_plan(dest: crate::game::hex::Hex) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![dest] }],
            final_pos: dest,
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // ── fires on canonical case ───────────────────────────────────────────────

    #[test]
    fn blindspot_fires_on_canonical_case() {
        // RANGED actor ends at (0,0). Enemy at (4,0), blocked by an ally at (2,0).
        // The ally occupies the line between actor and enemy — no LoS.
        let actor_pos = hex_from_offset(0, 0);
        let final_pos = hex_from_offset(0, 0); // stays in place
        let ally_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();

        let snap = BattleSnapshot::new(vec![actor.clone(), ally, enemy], 1);
        let content = empty_content();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = move_plan(final_pos);
        let ann = PlanAnnotation::default();
        let result = BlindspotRanged.evaluate(&plan, &ann, &ctx);

        assert!(result.is_some(), "critic must fire: RANGED with no LoS to enemy");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::BlindspotRanged);
        assert!(
            (hit.multiplier - BLINDSPOT_MULTIPLIER).abs() < 1e-6,
            "multiplier must be {BLINDSPOT_MULTIPLIER}, got {}", hit.multiplier
        );
        if let CriticReason::BlindspotRanged { enemies_visible } = hit.reason {
            assert_eq!(enemies_visible, 0);
        } else {
            panic!("expected BlindspotRanged reason, got {:?}", hit.reason);
        }
    }

    // ── passes on clean plan ──────────────────────────────────────────────────

    #[test]
    fn blindspot_passes_on_clean_plan() {
        // RANGED actor ends at (0,0). Enemy at (3,0) with no blocker in between.
        let actor_pos = hex_from_offset(0, 0);
        let final_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(3, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let content = empty_content();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let plan = move_plan(final_pos);
        let ann = PlanAnnotation::default();
        let result = BlindspotRanged.evaluate(&plan, &ann, &ctx);

        assert!(result.is_none(), "critic must not fire: RANGED with clear LoS to enemy");
    }

    // ── gate conditions as "scaling" contract ─────────────────────────────────

    #[test]
    fn blindspot_severity_scales_with_input() {
        // 1) Non-RANGED actor without LoS → None (gate: RANGED tag required).
        // 2) RANGED actor with no enemies → None (gate: enemies required).
        let actor_pos = hex_from_offset(0, 0);
        let final_pos = hex_from_offset(0, 0);
        let ally_blocker_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);

        let content = empty_content();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ann = PlanAnnotation::default();
        let plan = move_plan(final_pos);

        // Case 1: melee actor (no RANGED tag), enemy blocked → must return None.
        let melee_actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_blocker_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();
        let snap_melee = BattleSnapshot::new(vec![melee_actor.clone(), ally, enemy], 1);
        let ctx_melee = make_scoring_ctx(&world, &snap_melee, &maps, &reservations, &melee_actor);
        assert!(
            BlindspotRanged.evaluate(&plan, &ann, &ctx_melee).is_none(),
            "non-RANGED actor must not trigger blindspot critic"
        );

        // Case 2: RANGED actor with empty enemy list → must return None.
        let ranged_actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let snap_no_enemies = BattleSnapshot::new(vec![ranged_actor.clone()], 1);
        let ctx_no_enemies = make_scoring_ctx(&world, &snap_no_enemies, &maps, &reservations, &ranged_actor);
        assert!(
            BlindspotRanged.evaluate(&plan, &ann, &ctx_no_enemies).is_none(),
            "RANGED actor with no enemies must not trigger blindspot critic"
        );
    }
}
