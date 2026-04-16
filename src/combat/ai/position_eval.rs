use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::role::AiRole;
use crate::game::hex::Hex;

/// Per-role weights for [danger, ally_support, opportunity].
/// All three maps are normalized to [0, 1] — weights express relative importance directly.
/// Escape (ally_support − danger) is NOT included here because it's a linear combo
/// of danger and support, not an independent signal. It's used only in ProtectSelf/retreat.
const POSITION_WEIGHTS: [[f32; 3]; 5] = [
    // Bruiser: wants opportunity, moderate danger avoidance, likes ally proximity
    [-1.2, 0.6, 1.2],
    // Archer: strongly avoids danger, moderate ally preference
    [-2.0, 0.7, 0.8],
    // Mage: cautious, seeks opportunity for spell targets
    [-1.8, 0.8, 1.2],
    // Support: very cautious, stays near allies, avoids frontline
    [-2.5, 1.3, 0.5],
    // Assassin: aggressive, mild danger respect
    [-0.9, 0.25, 1.8],
];

fn role_index(role: AiRole) -> usize {
    match role {
        AiRole::Bruiser => 0,
        AiRole::Archer => 1,
        AiRole::Mage => 2,
        AiRole::Support => 3,
        AiRole::Assassin => 4,
    }
}

/// Evaluate how desirable `tile` is for a unit with the given `role`.
/// Combines all four influence maps using role-specific weights.
/// Higher = better position.
pub fn evaluate_position(tile: Hex, role: AiRole, maps: &InfluenceMaps) -> f32 {
    let w = &POSITION_WEIGHTS[role_index(role)];
    w[0] * maps.danger.get(tile)
        + w[1] * maps.ally_support.get(tile)
        + w[2] * maps.opportunity.get(tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::influence::InfluenceMap;
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
        let support_score = evaluate_position(h, AiRole::Support, &maps);
        let bruiser_score = evaluate_position(h, AiRole::Bruiser, &maps);
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
            assert_eq!(evaluate_position(h, role, &maps), 0.0);
        }
    }

    #[test]
    fn escape_does_not_affect_position_eval() {
        // position_eval uses only danger, ally_support, opportunity.
        // Changing escape should not change the score.
        let h = hex_from_offset(3, 3);
        let mut maps1 = maps_with_danger(h, 0.5);
        maps1.opportunity.add(h, 0.8);
        let mut maps2 = maps1.clone();
        maps2.escape.add(h, 0.9);

        for role in [AiRole::Bruiser, AiRole::Archer, AiRole::Mage, AiRole::Support, AiRole::Assassin] {
            assert_eq!(
                evaluate_position(h, role, &maps1),
                evaluate_position(h, role, &maps2),
                "escape should not affect position_eval for {:?}",
                role,
            );
        }
    }
}
