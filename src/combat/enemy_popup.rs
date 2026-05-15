use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{Faction, Team};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use bevy::prelude::*;

/// Cursor tracking which CombatLog events have been checked for popups.
#[derive(Resource, Default)]
pub struct PopupCursor(pub usize);

/// Runs in the combat chain after apply_effects.
/// Detects enemy ability usage and queues a popup with results.
pub fn queue_enemy_popup(
    log: Res<CombatLog>,
    mut cursor: ResMut<PopupCursor>,
    names: Query<&Name>,
    factions: Query<&Faction>,
    content: Res<ActiveContent>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    let events = &log.0[cursor.0..];
    cursor.0 = log.0.len();

    if events.is_empty() {
        return;
    }

    // Find popup-worthy events: enemy ability use and any phase transition.
    let mut i = 0;
    while i < events.len() {
        // Phase transitions: one self-contained popup per event.
        if let CombatEvent::PhaseEntered { actor: _, prev_name, next_name, flavor } = &events[i] {
            let mut lines = vec![
                prev_name.clone(),
                "───────────────".into(),
                format!("Новая фаза: {next_name}"),
            ];
            if let Some(flavor) = flavor {
                if !flavor.is_empty() {
                    lines.push("───────────────".into());
                    lines.push(flavor.clone());
                }
            }
            anim_queue.0.push_back(PendingAnim::Popup { lines });
            i += 1;
            continue;
        }

        let CombatEvent::AbilityUsed {
            actor,
            ability_name,
            target,
            target_pos,
            is_aoe,
            cost_str,
        } = &events[i]
        else {
            i += 1;
            continue;
        };

        // Only show popup for enemy actions.
        let is_enemy = factions
            .get(*actor)
            .is_ok_and(|f| f.0 == Team::Enemy);
        if !is_enemy {
            i += 1;
            continue;
        }

        let name =
            |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();

        let mut lines = Vec::new();
        lines.push(name(*actor));
        lines.push("───────────────".into());

        let costs = if cost_str.is_empty() {
            String::new()
        } else {
            format!(" [{}]", cost_str)
        };
        let target_label = if *is_aoe {
            let [q, r] = crate::game::hex::hex_to_offset(*target_pos);
            format!("({},{})", q, r)
        } else {
            name(*target)
        };
        lines.push(format!("{} → {}{}", ability_name, target_label, costs));

        // Collect subsequent result events (damage, heal, status, death).
        let mut j = i + 1;
        while j < events.len() {
            match &events[j] {
                CombatEvent::DamageResult {
                    target,
                    raw,
                    armor_reduced,
                    final_damage,
                } => {
                    let armor_part = if *armor_reduced > 0 {
                        format!(", броня -{armor_reduced}")
                    } else {
                        String::new()
                    };
                    lines.push(format!(
                        "Урон: {raw}{armor_part} → -{final_damage} HP ({})",
                        name(*target)
                    ));
                }
                CombatEvent::HealResult {
                    target,
                    formula,
                    amount,
                } => {
                    lines.push(format!(
                        "Лечение: {} → +{} HP ({})",
                        formula, amount, name(*target)
                    ));
                }
                CombatEvent::StatusApplied { target, status } => {
                    let sname = content
                        .statuses
                        .get(status)
                        .map_or(status.0.as_str(), |s| s.name.as_str());
                    lines.push(format!("{} получает «{}»", name(*target), sname));
                }
                CombatEvent::UnitDied { entity } => {
                    lines.push(format!("{} погиб!", name(*entity)));
                }
                CombatEvent::RageGained { .. } | CombatEvent::ManaChanged { .. } => {
                    // Skip resource change events in popup.
                }
                _ => break,
            }
            j += 1;
        }

        anim_queue.0.push_back(PendingAnim::Popup { lines });

        i = j;
    }
}
