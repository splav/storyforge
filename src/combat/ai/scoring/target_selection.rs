use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::world::tags::AiTags;

/// Pick the enemy with the highest `target_selection_score` relative to `active`.
/// Single source of truth for "what's the most important enemy right now".
pub fn highest_priority_enemy<'a>(
    active: &UnitSnapshot,
    snap: &'a BattleSnapshot,
) -> Option<&'a UnitSnapshot> {
    snap.enemies_of(active.team).max_by(|a, b| {
        target_selection_score(active, a, snap)
            .partial_cmp(&target_selection_score(active, b, snap))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Score how important `target` is as a selection priority relative to `active`.
///
/// Returns a value in 0..1 range. This is a *relative ranking* — the score
/// of unit A cannot be compared directly to unit B from a different actor's
/// perspective or snapshot.
pub fn target_selection_score(
    active: &UnitSnapshot,
    target: &UnitSnapshot,
    snap: &BattleSnapshot,
) -> f32 {
    let max_threat = snap
        .units
        .iter()
        .map(|u| u.threat)
        .fold(0.0f32, f32::max)
        .max(1.0);

    let threat = target.threat / max_threat;
    let killability = target.killability();
    let eff_hp = target.eff_hp() as f32;

    // Threat density: damage output per HP-to-kill. Captures "ROI per HP burned"
    // — a low-HP assassin is much more efficient to finish than a tank with
    // equal threat but more effective HP.
    let max_density = snap
        .units
        .iter()
        .map(|u| u.threat / (u.eff_hp().max(1)) as f32)
        .fold(0.0f32, f32::max)
        .max(0.01);
    let density = (target.threat / eff_hp.max(1.0)) / max_density;

    let vulnerability = if target.tags.contains(AiTags::LOW_HP) {
        0.3
    } else {
        0.0
    } + if target.damage_taken_bonus > 0 {
        0.2
    } else {
        0.0
    };

    // Role value comes from the composed axis profile: Support ≈ 1.0,
    // Control ≈ 0.8, Ranged ≈ 0.7, Melee ≈ 0.5, Tank ≈ 0.3.
    let role_value = target.role.role_value();

    let dist = active.pos.unsigned_distance_to(target.pos) as f32;
    let proximity = 1.0 / (1.0 + dist);

    let raw = threat * 0.20
        + killability * 0.20
        + density * 0.20
        + vulnerability * 0.15
        + role_value * 0.10
        + proximity * 0.15;
    raw.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::role::AxisProfile;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{unit, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn wounded_target_scores_higher() {
        let active = unit(0, Team::Player, hex_from_offset(0, 0));
        let healthy = unit(1, Team::Enemy, hex_from_offset(2, 2));
        let wounded = UnitBuilder::new(2, Team::Enemy, hex_from_offset(2, 2))
            .hp(5)
            .tags(AiTags::LOW_HP)
            .build();

        let s = BattleSnapshot::new(vec![active.clone(), healthy.clone(), wounded.clone()], 1);
        let ph = target_selection_score(&active, &healthy, &s);
        let pw = target_selection_score(&active, &wounded, &s);
        assert!(pw > ph, "wounded target should have higher priority");
    }

    #[test]
    fn support_target_scores_higher_than_bruiser() {
        let active = unit(0, Team::Player, hex_from_offset(0, 0));
        let support = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .role(AxisProfile { support: 1.0, ..Default::default() })
            .build();
        let bruiser = unit(2, Team::Enemy, hex_from_offset(3, 3));

        let s = BattleSnapshot::new(vec![active.clone(), support.clone(), bruiser.clone()], 1);
        let ps = target_selection_score(&active, &support, &s);
        let pb = target_selection_score(&active, &bruiser, &s);
        assert!(ps > pb, "support should be higher priority than bruiser");
    }
}
