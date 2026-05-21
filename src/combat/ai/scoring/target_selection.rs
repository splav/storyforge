use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::world::tags::AiTags;

pub fn highest_priority_enemy<'a>(
    active: UnitView<'_>,
    snap: &'a BattleSnapshot,
) -> Option<UnitView<'a>> {
    snap.enemies_of(active.team).max_by(|a, b| {
        target_selection_score(active, *a, snap)
            .partial_cmp(&target_selection_score(active, *b, snap))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

pub fn target_selection_score(
    active: UnitView<'_>,
    target: UnitView<'_>,
    snap: &BattleSnapshot,
) -> f32 {
    let max_threat = snap
        .cache
        .units
        .iter()
        .map(|u| u.threat)
        .fold(0.0f32, f32::max)
        .max(1.0);

    let threat = target.cache.threat / max_threat;
    let killability = target.killability();
    let eff_hp = target.eff_hp() as f32;

    // Threat density: damage output per HP-to-kill.
    let max_density = snap
        .state
        .units()
        .iter()
        .filter_map(|u| {
            // B-prime: use explicit translation map; `Entity::from_bits(u.id.0)`
            // panics for summons with synthetic UnitIds.
            let entity = snap.entity_for_uid(u.id)?;
            let c = snap.cache.unit(entity)?;
            let eff = (u.hp + u.armor + u.armor_bonus).max(1) as f32;
            Some(c.threat / eff)
        })
        .fold(0.0f32, f32::max)
        .max(0.01);
    let density = (target.cache.threat / eff_hp.max(1.0)) / max_density;

    let vulnerability = if target.cache.tags.contains(AiTags::LOW_HP) {
        0.3
    } else {
        0.0
    } + if target.damage_taken_bonus > 0 {
        0.2
    } else {
        0.0
    };

    let role_value = target.cache.role.role_value();

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
    
    use crate::combat::ai::test_helpers::{unit, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
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

        let s = snapshot_from(vec![active.clone(), healthy.clone(), wounded.clone()], 1);
        let va = s.unit(active.entity).unwrap();
        let vh = s.unit(healthy.entity).unwrap();
        let vw = s.unit(wounded.entity).unwrap();
        let ph = target_selection_score(va, vh, &s);
        let pw = target_selection_score(va, vw, &s);
        assert!(pw > ph, "wounded target should have higher priority");
    }

    #[test]
    fn support_target_scores_higher_than_bruiser() {
        let active = unit(0, Team::Player, hex_from_offset(0, 0));
        let support = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .role(AxisProfile { support: 1.0, ..Default::default() })
            .build();
        let bruiser = unit(2, Team::Enemy, hex_from_offset(3, 3));

        let s = snapshot_from(vec![active.clone(), support.clone(), bruiser.clone()], 1);
        let va = s.unit(active.entity).unwrap();
        let vs = s.unit(support.entity).unwrap();
        let vb = s.unit(bruiser.entity).unwrap();
        let ps = target_selection_score(va, vs, &s);
        let pb = target_selection_score(va, vb, &s);
        assert!(ps > pb, "support should be higher priority than bruiser");
    }
}
