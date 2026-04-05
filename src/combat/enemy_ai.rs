use bevy::prelude::*;
use crate::core::DiceRng;
use crate::game::components::{Abilities, Combatant, Faction, Team, Vital};
use crate::game::messages::UseAbility;
use crate::game::resources::CombatContext;

/// Automatically acts on behalf of enemy-controlled combatants.
/// Picks a random ability and a random living player target.
pub fn enemy_ai_system(
    ctx: Res<CombatContext>,
    mut rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    combatants: Query<(Entity, &Faction, &Abilities, &Vital), With<Combatant>>,
) {
    let Some(actor) = ctx.active else { return };
    let Ok((_, faction, abilities, vital)) = combatants.get(actor) else { return };
    if faction.0 != Team::Enemy { return; }
    if !vital.is_alive() { return; }
    if abilities.0.is_empty() { return; }

    // Random ability.
    let ability_idx = rng.roll_d(abilities.0.len() as u32) as usize - 1;
    let ability = abilities.0[ability_idx];

    // Collect living players, pick one at random.
    let players: Vec<Entity> = combatants
        .iter()
        .filter(|(_, f, _, v)| f.0 == Team::Player && v.is_alive())
        .map(|(e, _, _, _)| e)
        .collect();

    if players.is_empty() { return; }

    let target_idx = rng.roll_d(players.len() as u32) as usize - 1;
    let target = players[target_idx];

    use_ability.write(UseAbility { actor, ability, target });
}
