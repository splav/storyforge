//! BlindspotRanged critic.
//!
//! Fires when a RANGED actor with at least one living enemy ends its turn at a
//! position with no line-of-sight to any of them (and no 1-step kite-out either).

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::world::tags::AiTags;
use crate::game::hex::{has_los, in_bounds};
use hexx::Hex;
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
        if !active.cache.tags.contains(AiTags::RANGED) {
            return None;
        }

        let enemies: Vec<_> = ctx.snap.enemies_of(active.team).collect();
        if enemies.is_empty() {
            return None;
        }

        // LoS blockers: live unit positions + static obstacles. Both block the
        // intermediate hexes of `has_los`; the function itself skips endpoints.
        let occupied: HashSet<_> = ctx
            .snap
            .state
            .units()
            .iter()
            .filter(|u| u.is_alive())
            .map(|u| u.pos)
            .collect();
        let obstacles = &ctx.snap.state.blocked_hexes;

        let final_pos = plan.final_pos;

        // `ignore` is treated as transparent — used so the kite-yard probe
        // doesn't get blocked by the actor's own final_pos (he moves away).
        let visible_from = |from: Hex, ignore: Option<Hex>| -> bool {
            enemies.iter().any(|e| {
                has_los(from, e.pos, |mid| {
                    (occupied.contains(&mid) || obstacles.contains(&mid))
                        && mid != from
                        && mid != e.pos
                        && Some(mid) != ignore
                })
            })
        };

        // 1. LOS from the final position — actor can shoot this turn.
        if visible_from(final_pos, None) {
            return None;
        }

        // 2. 1-step lookahead (hide-then-shoot): if a walkable neighbour has LOS
        //    to an enemy, the actor can kite out, fire, and step back. Walkable =
        //    in-bounds, not an obstacle, not occupied. final_pos is transparent
        //    for the probe since the actor won't be standing there when it kites.
        let neighbour_has_los = final_pos.all_neighbors().iter().any(|&n| {
            in_bounds(n)
                && !obstacles.contains(&n)
                && !occupied.contains(&n)
                && visible_from(n, Some(final_pos))
        });
        if neighbour_has_los {
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
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::test_helpers::{
        assert_stage_critic_fires, assert_stage_critic_passes, StageTestHarness, UnitBuilder,
    };
    use crate::combat::ai::world::tags::AiTags;
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
        let actor_pos = hex_from_offset(0, 0);
        let ally_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![ally, enemy];

        assert_stage_critic_fires(
            &h,
            vec![move_plan(actor_pos)],
            BlindspotRanged,
            CriticKind::BlindspotRanged,
            BLINDSPOT_MULTIPLIER,
            |reason| {
                let CriticReason::BlindspotRanged { enemies_visible } = reason else {
                    panic!("expected BlindspotRanged reason, got {reason:?}");
                };
                assert_eq!(*enemies_visible, 0);
            },
        );
    }

    // ── passes on clean plan ──────────────────────────────────────────────────

    #[test]
    fn blindspot_passes_on_clean_plan() {
        // RANGED actor ends at (0,0). Enemy at (3,0) with no blocker — clear LoS.
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(3, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        let mut h = StageTestHarness::new(actor);
        h.extra_units = vec![enemy];

        assert_stage_critic_passes(&h, vec![move_plan(actor_pos)], BlindspotRanged);
    }

    // ── gate conditions as "scaling" contract ─────────────────────────────────

    #[test]
    fn blindspot_severity_scales_with_input() {
        // 1) Non-RANGED actor without LoS → no hit (gate: RANGED tag required).
        // 2) RANGED actor with no enemies → no hit (gate: enemies required).
        let actor_pos = hex_from_offset(0, 0);
        let ally_blocker_pos = hex_from_offset(2, 0);
        let enemy_pos = hex_from_offset(4, 0);
        let plan = vec![move_plan(actor_pos)];

        // Case 1: melee actor, enemy blocked → no hit.
        let melee_actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_blocker_pos).build();
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos).build();
        let mut h1 = StageTestHarness::new(melee_actor);
        h1.extra_units = vec![ally, enemy];
        assert_stage_critic_passes(&h1, plan.clone(), BlindspotRanged);

        // Case 2: RANGED actor, no enemies → no hit.
        let ranged_actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .tags(AiTags::RANGED)
            .build();
        let h2 = StageTestHarness::new(ranged_actor);
        assert_stage_critic_passes(&h2, plan, BlindspotRanged);
    }

    // ── 1-step lookahead: kite-yard prevents the critic from firing ───────────

    #[test]
    fn name_is_stable() {
        assert_eq!(BlindspotRanged.name(), "blindspot_ranged");
    }
}
