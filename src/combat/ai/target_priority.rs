use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};

/// Pick the enemy with the highest `target_priority` relative to `active`.
/// Single source of truth for "what's the most important enemy right now".
pub fn highest_priority_enemy<'a>(
    active: &UnitSnapshot,
    snap: &'a BattleSnapshot,
) -> Option<&'a UnitSnapshot> {
    snap.enemies_of(active.team).max_by(|a, b| {
        target_priority(active, a, snap)
            .partial_cmp(&target_priority(active, b, snap))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Score how important `target` is as a priority for `active`.
/// Returns a value in 0..1 range.
pub fn target_priority(
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
    // Killability uses effective HP (hp + armor) — the real remaining barrier
    // before this target dies. A well-armored tank at low HP is less killable
    // than an unarmored mage at the same HP%.
    let eff_hp = target.eff_hp() as f32;
    let eff_max = target.eff_max_hp() as f32;
    let killability = 1.0 - eff_hp / eff_max;

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
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::game::hex::hex_from_offset;

    fn unit(id: u32, team: crate::game::components::Team, pos: crate::game::hex::Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: bevy::prelude::Entity::from_raw_u32(id).expect("valid"),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action: true,
            movement: true,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 0,
            summoner: None,
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        let active = units[0].entity;
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    #[test]
    fn wounded_target_scores_higher() {
        use crate::game::components::Team;
        let active = unit(0, Team::Player, hex_from_offset(0, 0));
        let healthy = unit(1, Team::Enemy, hex_from_offset(2, 2));
        let mut wounded = unit(2, Team::Enemy, hex_from_offset(2, 2));
        wounded.hp = 5;
        wounded.tags = AiTags::LOW_HP;

        let s = snap(vec![active.clone(), healthy.clone(), wounded.clone()]);
        let ph = target_priority(&active, &healthy, &s);
        let pw = target_priority(&active, &wounded, &s);
        assert!(pw > ph, "wounded target should have higher priority");
    }

    #[test]
    fn support_target_scores_higher_than_bruiser() {
        use crate::game::components::Team;
        let active = unit(0, Team::Player, hex_from_offset(0, 0));

        let mut support = unit(1, Team::Enemy, hex_from_offset(3, 3));
        support.role = AxisProfile::from(AiRole::Support);
        let bruiser = unit(2, Team::Enemy, hex_from_offset(3, 3));

        let s = snap(vec![active.clone(), support.clone(), bruiser.clone()]);
        let ps = target_priority(&active, &support, &s);
        let pb = target_priority(&active, &bruiser, &s);
        assert!(ps > pb, "support should be higher priority than bruiser");
    }
}
