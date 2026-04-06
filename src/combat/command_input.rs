use bevy::prelude::*;
use crate::content::abilities::TargetType;
use crate::game::components::{Abilities, Combatant, Dead, Faction, Team, Vital};
use crate::game::messages::UseAbility;
use crate::game::resources::{CombatContext, GameDb, SelectionState};

pub fn player_command_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    mut selection: ResMut<SelectionState>,
    mut use_ability: MessageWriter<UseAbility>,
    combatants: Query<(Entity, &Vital, &Faction, &Abilities), (With<Combatant>, Without<Dead>)>,
) {
    let Some(actor) = ctx.active else { return };

    let Ok((_, _, faction, abilities)) = combatants.get(actor) else { return };
    if faction.0 != Team::Player { return; }

    if selection.selected_actor != Some(actor) {
        selection.selected_actor = Some(actor);
        selection.selected_ability = abilities.0.first().cloned();
        if let Some(ref id) = selection.selected_ability.clone() {
            if let Some(def) = db.abilities.get(id.0.as_str()) {
                if def.target_type == TargetType::Myself {
                    selection.selected_target = Some(actor);
                }
            }
        }
    }

    // Ability slots: 1 → abilities[0], 2 → abilities[1], …
    let slot_keys = [
        KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3,
        KeyCode::Digit4, KeyCode::Digit5,
    ];
    for (i, &key) in slot_keys.iter().enumerate() {
        if keyboard.just_pressed(key) {
            if let Some(ability_id) = abilities.0.get(i).cloned() {
                if let Some(def) = db.abilities.get(ability_id.0.as_str()) {
                    if def.target_type == TargetType::Myself {
                        selection.selected_target = Some(actor);
                    }
                }
                selection.selected_ability = Some(ability_id);
            }
        }
    }

    // Tab → cycle living targets (enemies for most abilities, allies for SingleAlly).
    if keyboard.just_pressed(KeyCode::Tab) {
        let is_single_ally = selection.selected_ability
            .as_ref()
            .and_then(|id| db.abilities.get(id.0.as_str()))
            .map_or(false, |def| def.target_type == crate::content::abilities::TargetType::SingleAlly);

        let candidates: Vec<Entity> = if is_single_ally {
            combatants
                .iter()
                .filter(|(_, v, f, _)| v.is_alive() && f.0 == Team::Player)
                .map(|(e, _, _, _)| e)
                .collect()
        } else {
            combatants
                .iter()
                .filter(|(e, v, f, _)| *e != actor && v.is_alive() && f.0 == Team::Enemy)
                .map(|(e, _, _, _)| e)
                .collect()
        };

        if !candidates.is_empty() {
            let current_idx = selection.selected_target
                .and_then(|t| candidates.iter().position(|&e| e == t))
                .unwrap_or(usize::MAX);
            selection.selected_target =
                Some(candidates[(current_idx.wrapping_add(1)) % candidates.len()]);
        }
    }

    // Enter → confirm.
    if keyboard.just_pressed(KeyCode::Enter) {
        if let (Some(ability), Some(target)) =
            (selection.selected_ability.clone(), selection.selected_target)
        {
            use_ability.write(UseAbility { actor, ability, target });
            selection.clear();
        }
    }
}
