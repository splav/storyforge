use crate::game::components::*;
use bevy::prelude::*;
use combat_engine::AbilityId;

#[derive(Bundle)]
pub struct CombatantBundle {
    pub combatant: Combatant,
    pub faction: Faction,
    pub stats: CombatStats,
    pub vital: Vital,
    pub runtime: RuntimeStatsMirror,
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
        magic_resist: i32,
        speed: i32,
        abilities: Vec<AbilityId>,
        equipment: Equipment,
    ) -> Self {
        let vital = Vital::new(&stats);
        Self {
            combatant: Combatant,
            faction: Faction(team),
            vital,
            stats,
            runtime: RuntimeStatsMirror(combat_engine::RuntimeStats {
                armor,
                magic_resist,
                base_speed: speed,
            }),
            initiative: Initiative(0),
            action_points: ActionPoints {
                action_points: 1,
                max_ap: 1,
                movement_points: speed,
            },
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
    magic_resist: i32,
    speed: i32,
    abilities: Vec<AbilityId>,
    equipment: Equipment,
) -> impl Bundle {
    (
        PartyMember,
        CombatantBundle::new(
            Team::Player,
            stats,
            armor,
            magic_resist,
            speed,
            abilities,
            equipment,
        ),
    )
}

pub fn enemy_bundle(
    stats: CombatStats,
    armor: i32,
    magic_resist: i32,
    speed: i32,
    abilities: Vec<AbilityId>,
    equipment: Equipment,
) -> impl Bundle {
    (
        Enemy,
        CombatantBundle::new(
            Team::Enemy,
            stats,
            armor,
            magic_resist,
            speed,
            abilities,
            equipment,
        ),
    )
}

/// Minimal non-acting NPC (e.g. a wounded ally to protect). Carries only what
/// the engine projection needs to see a unit; `Abilities`/`CombatStats`/
/// `Equipment` are omitted (AI snapshot defaults them to threat 0, no attacks).
/// Caller applies a perma-stun separately and adds `Name`/position.
pub fn npc_bundle(team: Team, vital: Vital) -> impl Bundle {
    (
        Combatant,
        Faction(team),
        vital,
        RuntimeStatsMirror(combat_engine::RuntimeStats {
            armor: 0,
            magic_resist: 0,
            base_speed: 0,
        }),
        Initiative(0),
        ActionPoints {
            action_points: 1,
            max_ap: 1,
            movement_points: 0,
        },
        Reactions::default(),
        StatusEffects::default(),
    )
}
