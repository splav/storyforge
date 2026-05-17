#![allow(clippy::too_many_arguments)]
use crate::content::content_view::ActiveContent;
use crate::combat::ai::system::has_ai_control_status;
use crate::content::abilities::{EffectDef, TargetType};
use crate::game::components::{ActiveCombatant, Combatant, Dead, PlayerCombatantQ, StatusEffects, Team};
use crate::game::messages::ActionInput;
use crate::game::resources::{HexPositions, SelectionState};
use bevy::prelude::*;

/// Map a single-char key string from TOML to a Bevy `KeyCode`.
fn key_str_to_keycode(key: &str) -> Option<KeyCode> {
    match key {
        "A" => Some(KeyCode::KeyA), "B" => Some(KeyCode::KeyB),
        "C" => Some(KeyCode::KeyC), "D" => Some(KeyCode::KeyD),
        "F" => Some(KeyCode::KeyF), "G" => Some(KeyCode::KeyG),
        "H" => Some(KeyCode::KeyH), "I" => Some(KeyCode::KeyI),
        "J" => Some(KeyCode::KeyJ), "K" => Some(KeyCode::KeyK),
        "L" => Some(KeyCode::KeyL), "M" => Some(KeyCode::KeyM),
        "N" => Some(KeyCode::KeyN), "O" => Some(KeyCode::KeyO),
        "P" => Some(KeyCode::KeyP), "Q" => Some(KeyCode::KeyQ),
        "R" => Some(KeyCode::KeyR), "S" => Some(KeyCode::KeyS),
        "T" => Some(KeyCode::KeyT), "U" => Some(KeyCode::KeyU),
        "V" => Some(KeyCode::KeyV), "W" => Some(KeyCode::KeyW),
        "X" => Some(KeyCode::KeyX), "Y" => Some(KeyCode::KeyY),
        "Z" => Some(KeyCode::KeyZ),
        _ => None,
    }
}

pub fn player_command_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    content: Res<ActiveContent>,
    positions: Res<HexPositions>,
    mut selection: ResMut<SelectionState>,
    mut action_input: MessageWriter<ActionInput>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<PlayerCombatantQ, (With<Combatant>, Without<Dead>)>,
    statuses: Query<&StatusEffects>,
) {
    let Ok(actor) = active_q.single() else { return };

    let Ok(c) = combatants.get(actor) else {
        return;
    };
    if c.faction.0 != Team::Player {
        return;
    }
    // Skip AI-controlled heroes (pact effect).
    if has_ai_control_status(actor, &statuses, &content) {
        return;
    }

    // Auto-enter move mode after GrantMovement (e.g. Rush) or whenever AP is
    // gone but MP still remains — movement is the only thing left to do.
    let no_ap_but_can_move = c.ap.action_points <= 0 && c.ap.can_move();
    if (c.has_bonus_move || no_ap_but_can_move) && !selection.move_mode {
        selection.move_mode = true;
        selection.selected_ability = None;
        selection.selected_target = None;
    }

    // Auto-end turn if both resources are spent.
    if c.ap.action_points <= 0 && !c.ap.can_move() {
        action_input.write(ActionInput::EndTurn { actor });
        selection.clear();
        return;
    }

    if selection.selected_actor != Some(actor) {
        selection.selected_actor = Some(actor);
        selection.selected_ability = c.abilities.0.first().cloned();
        selection.move_mode = false;
        if let Some(ref id) = selection.selected_ability.clone() {
            if let Some(def) = content.abilities.get(id.0.as_str()) {
                if def.target_type == TargetType::Myself {
                    selection.selected_target = Some(actor);
                }
            }
        }
    }

    // Custom-keyed abilities (universal: move, rest, etc.).
    for keyed_id in &content.keyed_abilities {
        let Some(def) = content.abilities.get(keyed_id) else { continue };
        let Some(ref key_str) = def.key else { continue };
        let Some(keycode) = key_str_to_keycode(key_str) else { continue };
        if !keyboard.just_pressed(keycode) {
            continue;
        }

        if matches!(def.effect, EffectDef::ToggleMoveMode) {
            if c.ap.can_move() {
                selection.move_mode = !selection.move_mode;
                if selection.move_mode {
                    selection.selected_ability = None;
                    selection.selected_target = None;
                }
            }
        } else if def.target_type == TargetType::Myself && c.ap.can_act_for(def.cost_ap) {
            let target_pos = positions.get(&actor).unwrap_or(hexx::Hex::ZERO);
            action_input.write(ActionInput::Cast {
                actor,
                ability: keyed_id.clone(),
                target: actor,
                target_pos,
            });
            selection.clear();
        }
        return;
    }

    // Numbered ability slots: 1 → class_abilities[0], 2 → class_abilities[1], …
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
                if let Some(def) = content.abilities.get(ability_id.0.as_str()) {
                    if def.target_type == TargetType::Myself {
                        selection.selected_target = Some(actor);
                    }
                }
                selection.selected_ability = Some(ability_id);
                selection.move_mode = false;
            }
        }
    }

    // Escape → cancel move mode.
    if keyboard.just_pressed(KeyCode::Escape) && selection.move_mode {
        selection.move_mode = false;
    }

    // E → manually end turn.
    if keyboard.just_pressed(KeyCode::KeyE) {
        action_input.write(ActionInput::EndTurn { actor });
        selection.clear();
        return;
    }

    // Tab → cycle living targets (enemies for most abilities, allies for SingleAlly).
    if keyboard.just_pressed(KeyCode::Tab) && !selection.move_mode {
        let target_type = selection
            .selected_ability
            .as_ref()
            .and_then(|id| content.abilities.get(id.0.as_str()))
            .map(|def| def.target_type);

        if matches!(target_type, Some(TargetType::Myself | TargetType::Ground)) {
            // self-cast / ground-targeted: Tab does nothing (no entity to cycle).
        } else {
            let is_single_ally = target_type == Some(TargetType::SingleAlly);

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
            let target_pos = positions.get(&target).unwrap_or(hexx::Hex::ZERO);
            action_input.write(ActionInput::Cast {
                actor,
                ability,
                target,
                target_pos,
            });
            selection.clear();
        }
    }
}
