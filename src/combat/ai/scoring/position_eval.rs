use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::config::tuning::AiTuning;
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::game::hex::Hex;

/// Evaluate how desirable `tile` is for a unit with the given profile.
/// Composed weights from `AxisProfile::position_weights()` combine the 3
/// influence maps (danger, ally_support, opportunity) — role emergent.
pub fn evaluate_position(
    tile: Hex,
    profile: &AxisProfile,
    tuning: &AiTuning,
    maps: &InfluenceMaps,
) -> f32 {
    let w = profile.position_weights(tuning);
    w[0] * maps.danger.get(tile)
        + w[1] * maps.ally_support.get(tile)
        + w[2] * maps.opportunity.get(tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::world::influence::InfluenceMap;
    use crate::game::hex::hex_from_offset;

    /// Five reference profiles spanning the axes — used to pin role-agnostic
    /// invariants (zero-map tiles, escape isolation). Pure axes on 4 of them
    /// and a tank/melee hybrid to represent frontline behaviour.
    fn sample_profiles() -> [AxisProfile; 5] {
        [
            AxisProfile {
                tank: 0.5,
                melee: 0.5,
                ..Default::default()
            }, // bruiser
            AxisProfile {
                ranged: 1.0,
                ..Default::default()
            }, // archer
            AxisProfile {
                ranged: 0.7,
                control: 0.3,
                ..Default::default()
            }, // mage
            AxisProfile {
                support: 1.0,
                ..Default::default()
            }, // support
            AxisProfile {
                melee: 0.8,
                tank: 0.2,
                ..Default::default()
            }, // assassin
        ]
    }

    fn maps_with_danger(hex: Hex, danger: f32) -> InfluenceMaps {
        let mut d = InfluenceMap::new();
        d.add(hex, danger);
        InfluenceMaps {
            danger: d,
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    #[test]
    fn support_avoids_danger_more_than_bruiser() {
        let h = hex_from_offset(3, 3);
        let maps = maps_with_danger(h, 0.7);
        let tuning = AiTuning::default();
        let support = AxisProfile {
            support: 1.0,
            ..Default::default()
        };
        let bruiser = AxisProfile {
            tank: 0.5,
            melee: 0.5,
            ..Default::default()
        };
        let support_score = evaluate_position(h, &support, &tuning, &maps);
        let bruiser_score = evaluate_position(h, &bruiser, &tuning, &maps);
        assert!(
            support_score < bruiser_score,
            "support should rate dangerous tile lower"
        );
    }

    #[test]
    fn safe_tile_scores_zero_for_all_roles() {
        let h = hex_from_offset(0, 0);
        let tuning = AiTuning::default();
        let maps = InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };
        for profile in sample_profiles() {
            assert_eq!(evaluate_position(h, &profile, &tuning, &maps), 0.0);
        }
    }

    #[test]
    fn escape_does_not_affect_position_eval() {
        // position_eval uses only danger, ally_support, opportunity.
        let h = hex_from_offset(3, 3);
        let tuning = AiTuning::default();
        let mut maps1 = maps_with_danger(h, 0.5);
        maps1.opportunity.add(h, 0.8);
        let mut maps2 = maps1.clone();
        maps2.escape.add(h, 0.9);

        for profile in sample_profiles() {
            assert_eq!(
                evaluate_position(h, &profile, &tuning, &maps1),
                evaluate_position(h, &profile, &tuning, &maps2),
                "escape should not affect position_eval for {:?}",
                profile,
            );
        }
    }
}
