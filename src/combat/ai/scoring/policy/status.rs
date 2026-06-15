//! Status effect value policies — HP-equivalent value of applied status effects.

use crate::combat::ai::scoring::horizon::{horizon_window_sum, status_applications};
use crate::content::abilities::AbilityDef;
use crate::content::content_view::ActiveContentData;

/// HP-equivalent value of a single stun-class status applied to `target` for
/// `duration` rounds.
///
/// Quantifies the projected damage denied to the team by locking `target` out
/// of their turns. Uses `damage_horizon` (DPR-correct); falls back to
/// `target.cache.threat × duration` on empty horizon (legacy logs / uninitialised
/// fixtures).
///
/// Caller is responsible for ensuring the status is a stun-class (`skips_turn`);
/// this function computes the denial value unconditionally from target + duration.
///
/// Extracted 1:1 from `scoring::stun_denial_value` inner per-status formula.
///
/// # Arguments
/// - `target` — the unit being stunned.
/// - `duration` — rounds the stun lasts.
pub fn stun_denial_value(
    target: crate::combat::ai::world::snapshot::UnitView<'_>,
    duration: f32,
) -> f32 {
    horizon_window_sum(target, duration)
}

/// HP-equivalent value of an armor-shred (or armor-buff) status applied for
/// `duration` rounds.
///
/// Formula: `armor_bonus.abs() × duration`.
///
/// Negative `armor_bonus` = shred on enemy; positive = buff on ally. Both valued
/// identically by `.abs()`.
///
/// Extracted 1:1 from `scoring::status_score` `armor_bonus` branch.
pub fn armor_shred_value(armor_bonus: i32, duration: f32) -> f32 {
    armor_bonus.abs() as f32 * duration
}

/// HP-equivalent value of all status effects applied by `def` on `target`.
///
/// Composite: sums stun denial + armor shred + DoT + %HP DoT +
/// silence (partial stun) + speed penalty across all status applications of `def`.
/// HP-equivalent scoring counts both signs of `armor_bonus` via `.abs()`.
///
/// Extracted 1:1 from `scoring::status_score`.
pub fn value(
    def: &AbilityDef,
    target: crate::combat::ai::world::snapshot::UnitView<'_>,
    content: &ActiveContentData,
) -> f32 {
    status_applications(def, content)
        .map(|(sd, d)| {
            let mut total = 0.0f32;
            // Stun: deny target's projected damage over `d` rounds.
            if sd.skips_turn {
                total += horizon_window_sum(target, d);
            }
            // Armor delta: negative = shred on enemy, positive = buff on ally.
            if sd.bonuses.runtime.0.armor != 0 {
                total += armor_shred_value(sd.bonuses.runtime.0.armor, d);
            }
            // DoT: expected tick damage × duration.
            if let Some(ref dice) = sd.dot_dice {
                total += dice.expected() * d;
            }
            // %HP DoT (e.g. exhaustion).
            if sd.hp_percent_dot > 0 {
                let tick_dmg = (target.max_hp() as f32 * sd.hp_percent_dot as f32 / 100.0).ceil();
                total += tick_dmg * d;
            }
            // Silence (blocks mana abilities): partial stun.
            if sd.blocks_mana_abilities {
                total += 0.5 * horizon_window_sum(target, d);
            }
            // Speed penalty: reduces tactical options.
            if sd.bonuses.runtime.0.base_speed < 0 {
                total += (-sd.bonuses.runtime.0.base_speed) as f32 * d;
            }
            total
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::combat::ai::world::snapshot::UnitView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn armor_shred_value_zero_for_zero_bonus() {
        assert_eq!(armor_shred_value(0, 3.0), 0.0);
    }

    #[test]
    fn armor_shred_value_scales_with_duration() {
        let v = armor_shred_value(4, 3.0);
        assert!((v - 12.0).abs() < 1e-6);
    }

    #[test]
    fn armor_shred_value_abs_symmetry() {
        assert!((armor_shred_value(2, 5.0) - armor_shred_value(-2, 5.0)).abs() < 1e-6);
    }

    #[test]
    fn stun_denial_uses_horizon_window() {
        let (u, c) = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .damage_horizon(vec![10.0, 15.0, 20.0])
            .build_pair();
        let target = UnitView {
            state: &u,
            cache: &c,
        };
        // Stun for 2 rounds → horizon_window_sum = 10 + 15 = 25.
        let v = stun_denial_value(target, 2.0);
        assert!((v - 25.0).abs() < 1e-6);
    }

    #[test]
    fn stun_denial_falls_back_to_threat_on_empty_horizon() {
        let (u, c) = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .threat(8.0)
            .build_pair();
        let target = UnitView {
            state: &u,
            cache: &c,
        };
        // damage_horizon empty → threat × duration = 8 × 3 = 24
        let v = stun_denial_value(target, 3.0);
        assert!((v - 24.0).abs() < 1e-6);
    }
}
