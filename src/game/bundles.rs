use crate::core::{AbilityId, WeaponId};
use crate::game::components::*;
use bevy::prelude::*;

#[derive(Bundle)]
pub struct CombatantBundle {
    pub combatant: Combatant,
    pub faction: Faction,
    pub stats: CombatStats,
    pub vital: Vital,
    pub initiative: Initiative,
    pub action_points: ActionPoints,
    pub abilities: Abilities,
    pub status_effects: StatusEffects,
    pub equipped_weapon: EquippedWeapon,
}

impl CombatantBundle {
    pub fn new(
        team: Team,
        stats: CombatStats,
        abilities: Vec<AbilityId>,
        weapon: WeaponId,
    ) -> Self {
        let vital = Vital::new(&stats);
        Self {
            combatant: Combatant,
            faction: Faction(team),
            vital,
            stats,
            initiative: Initiative(0),
            action_points: ActionPoints::default(),
            abilities: Abilities(abilities),
            status_effects: StatusEffects::default(),
            equipped_weapon: EquippedWeapon(weapon),
        }
    }
}

pub fn warrior_bundle(
    stats: CombatStats,
    abilities: Vec<AbilityId>,
    weapon: WeaponId,
) -> impl Bundle {
    (
        PartyMember,
        CombatantBundle::new(Team::Player, stats, abilities, weapon),
    )
}

pub fn enemy_bundle(
    stats: CombatStats,
    abilities: Vec<AbilityId>,
    weapon: WeaponId,
) -> impl Bundle {
    (
        Enemy,
        CombatantBundle::new(Team::Enemy, stats, abilities, weapon),
    )
}
