use crate::content::abilities::TargetType;
use crate::game::components::{Abilities, ActionPoints, BonusMovement, Combatant, Dead, Faction, Team, Vital};
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
    combatants: Query<
        (Entity, &Vital, &Faction, &Abilities, &ActionPoints, Has<BonusMovement>),
        (With<Combatant>, Without<Dead>),
    >,
) {
    let Some(actor) = ctx.active else { return };

    let Ok((_, _, faction, abilities, ap, has_bonus_move)) = combatants.get(actor) else {
        return;
    };
    if faction.0 != Team::Player {
        return;
    }

    // Auto-enter move mode after GrantMovement (e.g. Rush).
    if has_bonus_move && ap.movement && !selection.move_mode {
        selection.move_mode = true;
        selection.selected_ability = None;
        selection.selected_target = None;
    }

    // Auto-end turn if both resources are spent.
    if !ap.action && !ap.movement {
        end_turn.write(EndTurn { actor });
        selection.clear();
        return;
    }

    if selection.selected_actor != Some(actor) {
        selection.selected_actor = Some(actor);
        selection.selected_ability = abilities.0.first().cloned();
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
            if let Some(ability_id) = abilities.0.get(i).cloned() {
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
    if keyboard.just_pressed(KeyCode::KeyM) && ap.movement {
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
                    .filter(|(_, v, f, _, _, _)| v.is_alive() && f.0 == Team::Player)
                    .map(|(e, _, _, _, _, _)| e)
                    .collect()
            } else {
                combatants
                    .iter()
                    .filter(|(e, v, f, _, _, _)| *e != actor && v.is_alive() && f.0 == Team::Enemy)
                    .map(|(e, _, _, _, _, _)| e)
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
        info!(
            "[CMD] Enter pressed: ability={:?}, target={:?}, ap.action={}, ap.movement={}, move_mode={}",
            selection.selected_ability,
            selection.selected_target,
            ap.action,
            ap.movement,
            selection.move_mode,
        );
        if let (Some(ability), Some(target)) = (
            selection.selected_ability.clone(),
            selection.selected_target,
        ) {
            info!("[CMD] → writing UseAbility: ability={:?}, target={:?}", ability, target);
            use_ability.write(UseAbility {
                actor,
                ability,
                target,
            });
            selection.clear();
        } else {
            info!("[CMD] → UseAbility NOT sent (missing ability or target)");
        }
    }
}
