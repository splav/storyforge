use crate::core::DiceRng;
use crate::game::components::{Abilities, Combatant, Faction, StatusEffects, Team, Vital};
use crate::game::messages::UseAbility;
use crate::game::resources::{CombatContext, GameDb};
use bevy::prelude::*;

/// Automatically acts on behalf of enemy-controlled combatants.
/// Picks a random ability and a random living player target.
/// Targets players with forces_targeting statuses first.
pub fn enemy_ai_system(
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    combatants: Query<(Entity, &Faction, &Abilities, &Vital), With<Combatant>>,
    statuses: Query<&StatusEffects>,
) {
    let Some(actor) = ctx.active else { return };
    let Ok((_, faction, abilities, vital)) = combatants.get(actor) else {
        return;
    };
    if faction.0 != Team::Enemy {
        return;
    }
    if !vital.is_alive() {
        return;
    }
    if abilities.0.is_empty() {
        return;
    }

    // Random ability.
    let ability_idx = rng.roll_d(abilities.0.len() as u32) as usize - 1;
    let ability = abilities.0[ability_idx].clone();

    // Collect living players.
    let players: Vec<Entity> = combatants
        .iter()
        .filter(|(_, f, _, v)| f.0 == Team::Player && v.is_alive())
        .map(|(e, _, _, _)| e)
        .collect();

    if players.is_empty() {
        return;
    }

    // If any living player has a forces_targeting status, enemies must target them.
    let forced: Vec<Entity> = players
        .iter()
        .copied()
        .filter(|&e| {
            statuses.get(e).map_or(false, |se| {
                se.0.iter().any(|s| {
                    db.statuses
                        .get(&s.id)
                        .map_or(false, |def| def.forces_targeting)
                })
            })
        })
        .collect();

    let pool = if forced.is_empty() { &players } else { &forced };
    let target_idx = rng.roll_d(pool.len() as u32) as usize - 1;
    let target = pool[target_idx];

    use_ability.write(UseAbility {
        actor,
        ability,
        target,
    });
}
