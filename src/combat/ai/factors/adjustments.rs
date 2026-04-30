//! Reservation-based coordination adjustments + crit-fail expected-value.

use super::{OffensiveFactors, ScoredStep};
use crate::combat::ai::utility::ScoringCtx;
use crate::content::abilities::AbilityDef;
use crate::content::races::CritFailEffect;
use crate::core::ResourceKind;

/// Coordination knob: overkill penalty + duplicate-CC.
/// Phase 6 removed `focus` and `position` as scored factors, so the
/// focus-fire bonus and tile-collision penalty were dropped here too.
pub(crate) fn apply_reservation_adjustments(
    step: &ScoredStep,
    off: &mut OffensiveFactors,
    ctx: &ScoringCtx,
) {
    let reservations = ctx.reservations;
    let snap = ctx.snap;
    let difficulty = ctx.world.difficulty;
    if let Some(target_ent) = step.target() {
        let reserved_dmg = reservations.reserved_damage(target_ent);
        if reserved_dmg > 0.0 {
            if let Some(target_unit) = snap.unit(target_ent) {
                let hp_left = target_unit.hp as f32 - reserved_dmg;
                if hp_left <= 0.0 {
                    // Team-mates already reserved lethal damage. Our hit is
                    // waste (apart from a crit-fail hedge). Scale damage AND
                    // kill together — previously `kill = 0.0` was absolute
                    // while damage leaked `mult` through, leaving overkill
                    // plans attractive whenever raw damage was high.
                    let mult = difficulty.overkill_multiplier();
                    off.damage *= mult;
                    off.kill_now *= mult;
                    off.kill_promised *= mult;
                }
            }
        }
        if reservations.has_reserved_cc(target_ent) {
            off.cc *= 0.15;
        }
    }
}

pub fn crit_fail_adjusted(
    score: f32,
    def: &AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
    match effect {
        CritFailEffect::ManaOverload => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score - chance * mana_cost
        }
        CritFailEffect::CircuitBreach => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score * (1.0 - chance) - chance * mana_cost * 0.5
        }
        _ => score * (1.0 - chance),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::content::content_view::ContentView;
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Shared scaffolding for the adjustments suite. Adjustment logic is
    /// actor-agnostic (reservation coordination only), so zero-caster
    /// defaults on the placeholder actor are enough.
    fn fixture() -> (ContentView, DifficultyProfile) {
        (
            ContentView::load_global_for_tests(),
            DifficultyProfile::hard(),
        )
    }

    /// Placeholder active for tests where `apply_reservation_adjustments`
    /// doesn't actually read the actor — the bundle requires one but the
    /// coordination logic is actor-agnostic. Any minimal unit works.
    fn placeholder_active() -> crate::combat::ai::world::snapshot::UnitSnapshot {
        UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build()
    }

    /// Overkill penalty must scale `damage` AND `kill` together. Previously
    /// `kill` was absolute-zeroed while `damage` retained the difficulty
    /// multiplier's share — leaving overkill plans attractive whenever raw
    /// damage was high.
    #[test]
    fn overkill_scales_damage_and_kill_uniformly() {
        let (content, diff) = fixture();
        let utility = make_test_ctx(&content, &diff);
        let mult = diff.overkill_multiplier();
        assert!(mult > 0.0 && mult < 1.0, "precondition: non-trivial multiplier");

        let target = UnitBuilder::new(99, Team::Player, hex_from_offset(1, 0))
            .hp(5)
            .build();
        let target_ent = target.entity;
        let snap = BattleSnapshot::new(vec![target.clone()], 1);
        let mut reservations = Reservations::default();
        // Reserve 10 HP of incoming damage against a 5-HP target — lethal.
        reservations.reserve_damage(target_ent, 10.0);

        let ability = AbilityId::from("melee_attack");
        let step = ScoredStep::Cast {
            ability: &ability,
            target: target_ent,
            target_pos: target.pos,
            caster_tile: hex_from_offset(0, 0),
        };
        let mut off = OffensiveFactors { damage: 8.0, heal: 0.0, kill_now: 1.0, kill_promised: 0.0, cc: 0.0 };
        let maps = empty_maps();
        let active = placeholder_active();
        let ctx = make_scoring_ctx(&utility, &snap, &maps, &reservations, &active);
        apply_reservation_adjustments(&step, &mut off, &ctx);

        assert!(
            (off.damage - 8.0 * mult).abs() < 1e-5,
            "damage must be scaled by overkill multiplier, got {}", off.damage,
        );
        assert!(
            (off.kill_now - mult).abs() < 1e-5,
            "kill_now must be scaled by the SAME multiplier (not zeroed), got {}", off.kill_now,
        );
        assert!(
            off.kill_promised.abs() < 1e-5,
            "kill_promised was 0 before adjustment, should stay 0, got {}", off.kill_promised,
        );
        // Sanity: the target is still reachable by entity after mutation.
        assert!(snap.unit(target_ent).is_some());
    }
}
