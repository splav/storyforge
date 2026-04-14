use crate::content::abilities::TargetType;
use crate::game::components::{ActiveCombatant, Combatant, Dead, PlayerCombatantQ, Team};
use crate::game::messages::{EndTurn, UseAbility};
use crate::game::resources::{CombatContext, GameDb, SelectionState};
use bevy::prelude::*;

pub fn player_command_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    mut selection: ResMut<SelectionState>,
    mut use_ability: MessageWriter<UseAbility>,
    mut end_turn: MessageWriter<EndTurn>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<PlayerCombatantQ, (With<Combatant>, Without<Dead>)>,
) {
    let Ok(actor) = active_q.single() else { return };

    let Ok(c) = combatants.get(actor) else {
        return;
    };
    if c.faction.0 != Team::Player {
        return;
    }

    // Auto-enter move mode after GrantMovement (e.g. Rush).
    if c.has_bonus_move && c.ap.movement && !selection.move_mode {
        selection.move_mode = true;
        selection.selected_ability = None;
        selection.selected_target = None;
    }

    // Auto-end turn if both resources are spent (and no EndTurn already sent).
    if !c.ap.action && !c.ap.movement && !ctx.turn_ending {
        end_turn.write(EndTurn { actor });
        selection.clear();
        return;
    }

    if selection.selected_actor != Some(actor) {
        selection.selected_actor = Some(actor);
        selection.selected_ability = c.abilities.0.first().cloned();
        selection.move_mode = false;
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
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
    ];
    for (i, &key) in slot_keys.iter().enumerate() {
        if keyboard.just_pressed(key) {
            if let Some(ability_id) = c.abilities.0.get(i).cloned() {
                if let Some(def) = db.abilities.get(ability_id.0.as_str()) {
                    if def.target_type == TargetType::Myself {
                        selection.selected_target = Some(actor);
                    }
                }
                selection.selected_ability = Some(ability_id);
                selection.move_mode = false;
            }
        }
    }

    // M → toggle move mode (preserves selected ability).
    if keyboard.just_pressed(KeyCode::KeyM) && c.ap.movement {
        selection.move_mode = !selection.move_mode;
    }

    // Escape → cancel move mode.
    if keyboard.just_pressed(KeyCode::Escape) && selection.move_mode {
        selection.move_mode = false;
    }

    // E → end turn manually.
    if keyboard.just_pressed(KeyCode::KeyE) {
        end_turn.write(EndTurn { actor });
        selection.clear();
        return;
    }

    // Tab → cycle living targets (enemies for most abilities, allies for SingleAlly).
    if keyboard.just_pressed(KeyCode::Tab) && !selection.move_mode {
        let target_type = selection
            .selected_ability
            .as_ref()
            .and_then(|id| db.abilities.get(id.0.as_str()))
            .map(|def| def.target_type);

        if matches!(target_type, Some(TargetType::Myself)) {
            // self-cast: Tab does nothing
        } else {
            let is_single_ally = target_type.map_or(false, |t| t == TargetType::SingleAlly);

            let candidates: Vec<Entity> = if is_single_ally {
                combatants
                    .iter()
                    .filter(|c| c.vital.is_alive() && c.faction.0 == Team::Player)
                    .map(|c| c.entity)
                    .collect()
            } else {
                combatants
                    .iter()
                    .filter(|c| c.entity != actor && c.vital.is_alive() && c.faction.0 == Team::Enemy)
                    .map(|c| c.entity)
                    .collect()
            };

            if !candidates.is_empty() {
                let current_idx = selection
                    .selected_target
                    .and_then(|t| candidates.iter().position(|&e| e == t))
                    .unwrap_or(usize::MAX);
                selection.selected_target =
                    Some(candidates[(current_idx.wrapping_add(1)) % candidates.len()]);
            }
        }
    }

    // Enter → confirm ability.
    if keyboard.just_pressed(KeyCode::Enter) {
        if let (Some(ability), Some(target)) = (
            selection.selected_ability.clone(),
            selection.selected_target,
        ) {
            use_ability.write(UseAbility {
                actor,
                ability,
                target,
            });
            selection.clear();
        }
    }
}
