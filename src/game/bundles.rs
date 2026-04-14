use crate::core::AbilityId;
use crate::game::components::*;
use bevy::prelude::*;

#[derive(Bundle)]
pub struct CombatantBundle {
    pub combatant: Combatant,
    pub faction: Faction,
    pub stats: CombatStats,
    pub vital: Vital,
    pub speed: Speed,
    pub initiative: Initiative,
    pub action_points: ActionPoints,
    pub abilities: Abilities,
    pub status_effects: StatusEffects,
    pub equipment: Equipment,
}

impl CombatantBundle {
    pub fn new(
        team: Team,
        stats: CombatStats,
        armor: i32,
        speed: i32,
        abilities: Vec<AbilityId>,
        equipment: Equipment,
    ) -> Self {
        let vital = Vital::new(&stats, armor);
        Self {
            combatant: Combatant,
            faction: Faction(team),
            vital,
            stats,
            speed: Speed(speed),
            initiative: Initiative(0),
            action_points: ActionPoints::default(),
            abilities: Abilities(abilities),
            status_effects: StatusEffects::default(),
            equipment,
        }
    }
}

pub fn hero_bundle(
    stats: CombatStats,
    armor: i32,
    speed: i32,
    abilities: Vec<AbilityId>,
    equipment: Equipment,
) -> impl Bundle {
    (
        PartyMember,
        CombatantBundle::new(Team::Player, stats, armor, speed, abilities, equipment),
    )
}

pub fn enemy_bundle(
    stats: CombatStats,
    armor: i32,
    speed: i32,
    abilities: Vec<AbilityId>,
    equipment: Equipment,
) -> impl Bundle {
    (
        Enemy,
        CombatantBundle::new(Team::Enemy, stats, armor, speed, abilities, equipment),
    )
}
