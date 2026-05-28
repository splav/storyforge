use combat_engine::AbilityId;
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
    pub reactions: Reactions,
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
            action_points: ActionPoints { action_points: 1, max_ap: 1, movement_points: speed },
            abilities: Abilities(abilities),
            status_effects: StatusEffects::default(),
            equipment,
            reactions: Reactions::default(),
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

/// Minimal non-acting NPC present on the field (e.g. a wounded ally to protect).
/// Carries only what the engine projection needs to see a unit (Faction+Vital,
/// plus trivial Speed/AP/Reactions to avoid `from_ecs` warns). `Abilities`,
/// `CombatStats`, and `Equipment` are intentionally omitted — the AI snapshot
/// (`AiCombatantQ`) now defaults them (threat 0, no attacks). Apply a
/// perma-stun status separately so it never takes a turn. Caller adds `Name`
/// and position components.
pub fn npc_bundle(team: Team, vital: Vital) -> impl Bundle {
    (
        Combatant,
        Faction(team),
        vital,
        Speed(0),
        Initiative(0),
        ActionPoints { action_points: 1, max_ap: 1, movement_points: 0 },
        Reactions::default(),
        StatusEffects::default(),
    )
}
