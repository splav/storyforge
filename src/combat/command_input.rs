use bevy::prelude::*;
use crate::content::abilities::TargetType;
use crate::game::components::{Abilities, Combatant, Dead, Faction, Team, Vital};
use crate::game::messages::UseAbility;
use crate::game::resources::{CombatContext, GameDb, SelectionState};

/// Keys:
///   1 → Sword Attack  (needs Tab to pick target, then Enter)
///   2 → Shield Block  (Enter immediately — self-targeted)
///   Tab → cycle enemy targets
///   Enter → confirm
pub fn player_command_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    ctx: Res<CombatContext>,
    db: Res<GameDb>,
    mut selection: ResMut<SelectionState>,
    mut use_ability: MessageWriter<UseAbility>,
    combatants: Query<(Entity, &Vital, &Faction, &Abilities), (With<Combatant>, Without<Dead>)>,
) {
    let Some(actor) = ctx.active else { return };

    // Only react to living player-controlled actors.
    let Ok((_, _, faction, abilities)) = combatants.get(actor) else { return };
    if faction.0 != Team::Player { return; }

    if selection.selected_actor != Some(actor) {
        selection.selected_actor = Some(actor);
        // Auto-select first ability.
        selection.selected_ability = abilities.0.first().copied();
        // Auto-set target for self-targeting abilities, keep target otherwise.
        if let Some(id) = selection.selected_ability {
            if let Some(def) = db.abilities.get(&id) {
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
            if let Some(&ability_id) = abilities.0.get(i) {
                selection.selected_ability = Some(ability_id);
                // Only override target for self-targeted abilities; keep existing target otherwise.
                if let Some(def) = db.abilities.get(&ability_id) {
                    if def.target_type == TargetType::Myself {
                        selection.selected_target = Some(actor);
                    }
                }
            }
        }
    }

    // Tab → cycle living (non-Dead) enemy targets.
    if keyboard.just_pressed(KeyCode::Tab) {
        let enemies: Vec<Entity> = combatants
            .iter()
            .filter(|(e, v, f, _)| *e != actor && v.is_alive() && f.0 == Team::Enemy)
            .map(|(e, _, _, _)| e)
            .collect();

        if !enemies.is_empty() {
            let current_idx = selection.selected_target
                .and_then(|t| enemies.iter().position(|&e| e == t))
                .unwrap_or(usize::MAX);
            selection.selected_target =
                Some(enemies[(current_idx.wrapping_add(1)) % enemies.len()]);
        }
    }

    // Enter → confirm.
    if keyboard.just_pressed(KeyCode::Enter) {
        if let (Some(ability), Some(target)) =
            (selection.selected_ability, selection.selected_target)
        {
            use_ability.write(UseAbility { actor, ability, target });
            selection.clear();
        }
    }
}
