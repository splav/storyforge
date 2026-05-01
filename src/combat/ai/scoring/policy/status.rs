//! Status effect value policies — HP-equivalent value of applied status effects.

use crate::combat::ai::scoring::horizon::{horizon_window_sum, status_applications};
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::content::abilities::AbilityDef;
use crate::content::content_view::ContentView;

/// HP-equivalent value of a single stun-class status applied to `target` for
/// `duration` rounds.
///
/// Quantifies the projected damage denied to the team by locking `target` out
/// of their turns. Uses `damage_horizon` (DPR-correct); falls back to
/// `target.threat × duration` on empty horizon (legacy logs / uninitialised
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
pub fn stun_denial_value(target: &UnitSnapshot, duration: f32) -> f32 {
    horizon_window_sum(target, duration)
}

/// HP-equivalent value of a vulnerability status applied for `duration` rounds.
///
/// Formula: `damage_taken_bonus.abs() × duration`.
///
/// Both positive (vulnerability on enemy) and negative (resistance on ally) are
/// valued by `.abs()` — a resistance buff is worth as much as a vulnerability debuff
/// of the same magnitude.
///
/// Extracted 1:1 from `scoring::status_score` `damage_taken_bonus` branch.
pub fn vulnerability_value(damage_taken_bonus: i32, duration: f32) -> f32 {
    damage_taken_bonus.abs() as f32 * duration
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
/// Composite: sums stun denial + vulnerability + armor shred + DoT + %HP DoT +
/// silence (partial stun) + speed penalty across all status applications of `def`.
/// HP-equivalent scoring counts both signs of `damage_taken_bonus` /
/// `armor_bonus` via `.abs()`.
///
/// Extracted 1:1 from `scoring::status_score`.
pub fn value(def: &AbilityDef, target: &UnitSnapshot, content: &ContentView) -> f32 {
    status_applications(def, content)
        .map(|(sd, d)| {
            let mut total = 0.0f32;
            // Stun: deny target's projected damage over `d` rounds.
            if sd.skips_turn {
                total += horizon_window_sum(target, d);
            }
            // Vulnerability: extra damage taken per hit for d rounds.
            if sd.damage_taken_bonus != 0 {
                total += vulnerability_value(sd.damage_taken_bonus, d);
            }
            // Armor delta: negative = shred on enemy, positive = buff on ally.
            if sd.armor_bonus != 0 {
                total += armor_shred_value(sd.armor_bonus, d);
            }
            // DoT: expected tick damage × duration.
            if let Some(ref dice) = sd.dot_dice {
                total += dice.expected() * d;
            }
            // %HP DoT (e.g. exhaustion).
            if sd.hp_percent_dot > 0 {
                let tick_dmg = (target.max_hp as f32 * sd.hp_percent_dot as f32 / 100.0).ceil();
                total += tick_dmg * d;
            }
            // Silence (blocks mana abilities): partial stun.
            if sd.blocks_mana_abilities {
                total += 0.5 * horizon_window_sum(target, d);
            }
            // Speed penalty: reduces tactical options.
            if sd.speed_bonus < 0 {
                total += (-sd.speed_bonus) as f32 * d;
            }
            total
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn db() -> ContentView {
        ContentView::load_global_for_tests()
    }

    fn base_target() -> UnitSnapshot {
        UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build()
    }

    #[test]
    fn vulnerability_value_zero_for_zero_bonus() {
        assert_eq!(vulnerability_value(0, 3.0), 0.0);
    }

    #[test]
    fn vulnerability_value_scales_with_duration() {
        let v = vulnerability_value(5, 4.0);
        assert!((v - 20.0).abs() < 1e-6);
    }

    #[test]
    fn vulnerability_value_abs_symmetry() {
        // Positive and negative bonus of same magnitude → same value.
        assert!((vulnerability_value(3, 2.0) - vulnerability_value(-3, 2.0)).abs() < 1e-6);
    }

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
        let mut target = base_target();
        target.damage_horizon = vec![10.0, 15.0, 20.0];
        // Stun for 2 rounds → horizon_window_sum = 10 + 15 = 25.
        let v = stun_denial_value(&target, 2.0);
        assert!((v - 25.0).abs() < 1e-6);
    }

    #[test]
    fn stun_denial_falls_back_to_threat_on_empty_horizon() {
        let mut target = base_target();
        target.threat = 8.0;
        // damage_horizon empty → threat × duration = 8 × 3 = 24
        let v = stun_denial_value(&target, 3.0);
        assert!((v - 24.0).abs() < 1e-6);
    }

    /// `policy::status::value` must be bit-identical to `scoring::status_score`
    /// for any (ability, target) pair.
    #[test]
    fn status_value_matches_scoring_status_score() {
        use crate::combat::ai::scoring::horizon::status_score;
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .max_hp(100)
            .hp(60)
            .build();

        // Test all abilities in content that have status applications.
        let mut tested = 0usize;
        for (id, def) in &content.abilities {
            if !def.statuses.is_empty() {
                let policy_val = value(def, &target, &content);
                let legacy_val = status_score(def, &target, &content);
                assert!(
                    (policy_val - legacy_val).abs() < 1e-6,
                    "status::value vs status_score diverge for {id:?}: policy={policy_val} legacy={legacy_val}"
                );
                tested += 1;
            }
        }
        // Ensure we actually tested something — if the content has no status
        // abilities this test would pass vacuously.
        assert!(tested > 0, "no status-applying abilities found in content; test is vacuous");
    }
}
