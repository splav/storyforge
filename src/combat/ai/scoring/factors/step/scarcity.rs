//! `StepFactor::Scarcity` — resource-vs-swing justification for Cast.
//!
//! Reads `outcome.p_kill_now` as the kill signal (same source as `KillNow`
//! factor) and delegates to `compute_scarcity`. Move steps return 0.0.
//!
//! # Contract on `ctx`
//! `ctx` must use the pre-step snapshot (shifted via `with_perspective` by the
//! caller). This matches legacy `compute_plan_factors_sans_intent` where the
//! scarcity call sat inside the `with_perspective` block.

pub const NAME: &str = "scarcity";
pub const SIGNED: bool = true;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::scoring::factors::ScoredStep;
use crate::combat::ai::scoring::factors::aoe_hits::aoe_hits;
use crate::combat::ai::scoring::factors::offensive::aoe_area;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::scoring::stun_denial_value;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::world::snapshot::{UnitSnapshot, UnitView};
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ContentView;

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    // Derive kill_now from the same outcome fact that KillNow uses.
    let kill_now = outcome.p_kill_now;
    compute_scarcity(step, kill_now, ctx)
}

/// Compute resource-scarcity factor: `swing_value - resource_ratio`.
/// Free abilities return 0.0 (neutral). Expensive abilities on low-value
/// situations get negative scores; expensive abilities in high-swing moments
/// get positive scores.
fn compute_scarcity(step: &ScoredStep, kill: f32, ctx: &ScoringCtx) -> f32 {
    let ScoredStep::Cast { ability, target_pos, target, caster_tile } = step else {
        return 0.0;
    };
    let world = ctx.world;
    let snap = ctx.snap;
    let active = ctx.active;
    let Some(def) = world.content.abilities.get(*ability) else {
        return 0.0;
    };

    // Free abilities are always neutral.
    if def.costs.is_empty() {
        return 0.0;
    }

    // resource_ratio: max(cost / current_pool) across all resource costs.
    let resource_ratio = def
        .costs
        .iter()
        .map(|c| {
            let pool = active.resource_amount(c.resource);
            if pool <= 0 {
                return 1.0;
            }
            (c.amount as f32 / pool as f32).min(1.0)
        })
        .fold(0.0f32, f32::max);

    // swing_value: situational justification for spending.
    let mut swing = 0.0f32;

    let target_unit = snap.unit_snapshot(*target);

    // Classify AoE hits once; both the victim pick and the multi-hit bonus
    // below read from the same list.
    let aoe_enemies: Vec<&UnitSnapshot> = if def.aoe == AoEShape::None {
        Vec::new()
    } else {
        let area = aoe_area(def, *target_pos, *caster_tile);
        aoe_hits(&area, active, snap).enemies
    };

    // Kill bonus.
    if kill > 0.0 {
        swing += 0.8;
        // Extra value for killing high-value targets. For AoE (target is
        // a sentinel), credit the highest-value enemy hit.
        let victim = target_unit.or_else(|| {
            aoe_enemies.iter().copied().max_by(|a, b| {
                a.role
                    .role_value()
                    .partial_cmp(&b.role.role_value())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        if let Some(t) = victim {
            // Role-based kill bonus scales with target's priority value
            // (Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3).
            swing += 0.35 * t.role.role_value();
        }
    }

    // AoE multi-hit bonus.
    if aoe_enemies.len() > 1 {
        swing += 0.2 * (aoe_enemies.len() - 1) as f32;
    }

    // CC on unstunned target. Non-AoE only — AoE CC is already folded into
    // the cc factor per-enemy. Reads `stun_denial_value` — the same helper
    // `status_cc_value` uses for its skips_turn contribution, so the scarcity
    // swing and cc factor value the stun on the same denominator and can
    // never drift. The duplication across two factors is intentional: `cc`
    // ranks plan utility, `scarcity` justifies the resource spend — two
    // orthogonal axes with different role weights, parallel to how `kill` is
    // mirrored below.
    if let Some(t) = target_unit {
        if !t.is_stunned(world.status_tags) {
            let stun_value = stun_denial_value(def, t, world.content);
            if stun_value > 0.0 {
                // `30.0` ≈ three strong rounds of DPR → full bonus saturation.
                swing += 0.5 * (stun_value / 30.0).min(1.0);
            }
        }
    }

    // Overkill penalty: target nearly dead and caster has free attacks.
    if let Some(t) = target_unit {
        if t.hp_pct() < 0.25 && has_free_attack(active, world.content) {
            swing -= 0.3;
        }
    }

    // Early round penalty: conserve resources at fight start.
    if snap.state.round <= 1 {
        swing -= 0.15;
    }

    (swing - resource_ratio).clamp(-1.0, 1.0)
}

/// Returns true if the caster has at least one ability with no resource cost.
/// Reads abilities from the actor's own cache — same source
/// `SnapshotActionState::actor_knows_ability` uses, so no dual-list drift.
fn has_free_attack(active: UnitView<'_>, content: &ContentView) -> bool {
    active.cache.abilities.iter().any(|id| {
        content
            .abilities
            .get(id)
            .is_some_and(|d| d.costs.is_empty() && d.target_type == TargetType::SingleEnemy)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::config::role::AxisProfile;
    use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, unit, UnitBuilder,
        snapshot_from,
    };
    use crate::combat::ai::orchestration::AiWorld;
    use crate::content::content_view::ContentView;
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use bevy::prelude::*;

    /// Shared scaffolding for the scarcity suite. All tests here are
    /// direction-only (assert < 0 / > 0 / == 0), so caster-mod tuning
    /// doesn't alter outcomes — `UnitBuilder` defaults suffice for the
    /// actor. If a future test needs INT-mod-sensitive behaviour, set
    /// `UnitBuilder::caster_ctx(...)` on the active unit explicitly.
    fn scarcity_fixture() -> (ContentView, DifficultyProfile) {
        (ContentView::load_global_for_tests(), DifficultyProfile::default())
    }

    fn cast_step<'a>(
        tile: Hex,
        ability: &'a AbilityId,
        target_pos: Hex,
        target: Entity,
    ) -> ScoredStep<'a> {
        ScoredStep::Cast { ability, target, target_pos, caster_tile: tile }
    }

    /// Score `compute_scarcity` against a freshly-built `ScoringCtx` bundle.
    /// Inlines the `empty_maps` + `Reservations::default()` scaffolding that
    /// every test in this suite needs after the bundle migration.
    fn score<'a>(
        step: &ScoredStep,
        kill: f32,
        ctx: &'a AiWorld<'a>,
        snap: &BattleSnapshot,
        active: &UnitSnapshot,
    ) -> f32 {
        let maps = empty_maps();
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(ctx, snap, &maps, &reservations, active);
        compute_scarcity(step, kill, &scoring)
    }

    #[test]
    fn scarcity_neutral_for_free_abilities() {
        let tile = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, tile);
        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        let s = snapshot_from(vec![active.clone(), enemy.clone()], 1);
        let (content, diff) = scarcity_fixture();
        let ctx = make_test_ctx(&content, &diff);

        let ab = AbilityId::from("melee_attack");
        let step = cast_step(tile, &ab, tile, enemy.entity);
        let score = score(&step, 0.0, &ctx, &s, &active);
        assert_eq!(score, 0.0, "free ability should have zero scarcity");
    }

    #[test]
    fn scarcity_penalizes_expensive_on_dying_target() {
        let tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(0, Team::Enemy, tile).mana(10, 10).build();
        let enemy = UnitBuilder::new(1, Team::Player, hex_from_offset(3, 3))
            .hp(1)
            .build();

        let s = snapshot_from(vec![active.clone(), enemy.clone()], 1);
        let (content, diff) = scarcity_fixture();
        let ctx = make_test_ctx(&content, &diff);

        let ab = AbilityId::from("fireball");
        let step = cast_step(tile, &ab, enemy.pos, enemy.entity);
        let score = score(&step, 0.0, &ctx, &s, &active);
        assert!(
            score < 0.0,
            "expensive ability on dying target should get negative scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_kill_on_support() {
        let tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(0, Team::Enemy, tile).mana(10, 10).build();
        let enemy = UnitBuilder::new(1, Team::Player, hex_from_offset(3, 3))
            .role(AxisProfile { support: 1.0, ..Default::default() })
            .hp(5)
            .build();

        let s = snapshot_from(vec![active.clone(), enemy.clone()], 1);
        let (content, diff) = scarcity_fixture();
        let ctx = make_test_ctx(&content, &diff);

        let ab = AbilityId::from("fireball");
        let step = cast_step(tile, &ab, enemy.pos, enemy.entity);
        let score = score(&step, 1.0, &ctx, &s, &active);
        assert!(
            score > 0.0,
            "kill on support should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_rewards_aoe_on_cluster() {
        let tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(0, Team::Enemy, tile).mana(20, 20).build();

        let center = hex_from_offset(2, 3);
        let neighbors: Vec<Hex> = center.all_neighbors().to_vec();
        let e1 = unit(1, Team::Player, center);
        let e2 = unit(2, Team::Player, neighbors[0]);
        let e3 = unit(3, Team::Player, neighbors[1]);

        let s = snapshot_from(
            vec![active.clone(), e1.clone(), e2.clone(), e3.clone()],
            3,
        );
        let (content, diff) = scarcity_fixture();
        let ctx = make_test_ctx(&content, &diff);

        let ab = AbilityId::from("fireball");
        let step = cast_step(tile, &ab, e1.pos, e1.entity);
        let score = score(&step, 0.0, &ctx, &s, &active);
        assert!(
            score > 0.0,
            "AoE on cluster should yield positive scarcity, got {:.2}",
            score,
        );
    }

    #[test]
    fn scarcity_penalizes_early_round_spend() {
        let tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(0, Team::Enemy, tile).mana(10, 10).build();
        let enemy = unit(1, Team::Player, hex_from_offset(3, 3));
        let (content, diff) = scarcity_fixture();
        let ctx = make_test_ctx(&content, &diff);

        let ab = AbilityId::from("fireball");
        let step = cast_step(tile, &ab, enemy.pos, enemy.entity);
        let pair = |round: u32| -> f32 {
            let s = snapshot_from(vec![active.clone(), enemy.clone()], round);
            score(&step, 0.0, &ctx, &s, &active)
        };
        let (score_r1, score_r3) = (pair(1), pair(3));
        assert!(
            score_r1 < score_r3,
            "round 1 ({:.2}) should have lower scarcity than round 3 ({:.2})",
            score_r1, score_r3,
        );
    }
}
