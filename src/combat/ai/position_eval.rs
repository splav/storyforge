use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::role::AxisProfile;
use crate::game::hex::Hex;

/// Evaluate how desirable `tile` is for a unit with the given profile.
/// Composed weights from `AxisProfile::position_weights()` combine the 3
/// influence maps (danger, ally_support, opportunity) — role emergent.
pub fn evaluate_position(tile: Hex, profile: &AxisProfile, maps: &InfluenceMaps) -> f32 {
    let w = profile.position_weights();
    w[0] * maps.danger.get(tile)
        + w[1] * maps.ally_support.get(tile)
        + w[2] * maps.opportunity.get(tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::influence::InfluenceMap;
    use crate::combat::ai::role::AiRole;
    use crate::game::hex::hex_from_offset;

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
        let support: AxisProfile = AiRole::Support.into();
        let bruiser: AxisProfile = AiRole::Bruiser.into();
        let support_score = evaluate_position(h, &support, &maps);
        let bruiser_score = evaluate_position(h, &bruiser, &maps);
        assert!(
            support_score < bruiser_score,
            "support should rate dangerous tile lower"
        );
    }

    #[test]
    fn safe_tile_scores_zero_for_all_roles() {
        let h = hex_from_offset(0, 0);
        let maps = InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };
        for role in [AiRole::Bruiser, AiRole::Archer, AiRole::Mage, AiRole::Support, AiRole::Assassin] {
            let profile: AxisProfile = role.into();
            assert_eq!(evaluate_position(h, &profile, &maps), 0.0);
        }
    }

    #[test]
    fn escape_does_not_affect_position_eval() {
        // position_eval uses only danger, ally_support, opportunity.
        let h = hex_from_offset(3, 3);
        let mut maps1 = maps_with_danger(h, 0.5);
        maps1.opportunity.add(h, 0.8);
        let mut maps2 = maps1.clone();
        maps2.escape.add(h, 0.9);

        for role in [AiRole::Bruiser, AiRole::Archer, AiRole::Mage, AiRole::Support, AiRole::Assassin] {
            let profile: AxisProfile = role.into();
            assert_eq!(
                evaluate_position(h, &profile, &maps1),
                evaluate_position(h, &profile, &maps2),
                "escape should not affect position_eval for {:?}",
                role,
            );
        }
    }
}
