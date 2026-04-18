//! Reservation-based coordination adjustments + crit-fail expected-value.

use super::OffensiveFactors;
use crate::combat::ai::candidates::ActionCandidate;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::AbilityDef;
use crate::content::races::CritFailEffect;
use crate::core::ResourceKind;

/// Coordination knob: overkill penalty + focus-fire bonus + duplicate-CC + tile collision.
pub(super) fn apply_reservation_adjustments(
    candidate: &ActionCandidate,
    off: &mut OffensiveFactors,
    focus: &mut f32,
    position: &mut f32,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
    reservations: &Reservations,
) {
    if let Some(target_ent) = candidate.target() {
        let reserved_dmg = reservations.reserved_damage(target_ent);
        if reserved_dmg > 0.0 {
            if let Some(target_unit) = snap.unit(target_ent) {
                let hp_left = target_unit.hp as f32 - reserved_dmg;
                if hp_left <= 0.0 {
                    off.damage *= ctx.difficulty.overkill_damage_multiplier();
                    off.kill = 0.0;
                } else {
                    *focus *= 1.0 + ctx.difficulty.focus_fire_bonus();
                }
            }
        }
        if reservations.has_reserved_cc(target_ent) {
            off.cc *= 0.15;
        }
    }
    if reservations.is_tile_reserved(candidate.tile) {
        *position *= 0.5;
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
