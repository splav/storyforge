use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::role::AiRole;
use crate::game::hex::Hex;

/// Per-role weights for [danger, ally_support, opportunity, escape].
const POSITION_WEIGHTS: [[f32; 4]; 5] = [
    // Bruiser: charges in, doesn't fear danger much
    [-0.3, 0.5, 1.0, 0.0],
    // Archer: avoids danger, seeks escape routes
    [-1.0, 0.3, 0.5, 0.8],
    // Mage: moderate caution, seeks opportunities
    [-0.8, 0.5, 0.8, 0.5],
    // Support: very cautious, stays near allies
    [-1.2, 1.0, 0.2, 1.0],
    // Assassin: fearless, seeks opportunity
    [-0.2, 0.1, 1.2, 0.0],
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
        + w[3] * maps.escape.get(tile)
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
        let maps = maps_with_danger(h, 10.0);
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
}
