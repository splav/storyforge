use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{facing_toward, Faction, Team};
use crate::game::resources::HexPositions;
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
        if let CombatEvent::PhaseEntered {
            actor: _,
            prev_name,
            next_name,
            flavor,
        } = &events[i]
        {
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
        let is_enemy = factions.get(*actor).is_ok_and(|f| f.0 == Team::Enemy);
        if !is_enemy {
            i += 1;
            continue;
        }

        let name = |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();

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
                CombatEvent::HealResult { target, amount } => {
                    lines.push(format!("Лечение: +{} HP ({})", amount, name(*target)));
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
                CombatEvent::PoolChanged { .. } => {
                    // Skip pool-change events in popup.
                }
                _ => break,
            }
            j += 1;
        }

        anim_queue.0.push_back(PendingAnim::Popup { lines });

        i = j;
    }
}

/// Cursor tracking which CombatLog events have been checked for victim facing.
#[derive(Resource, Default)]
pub struct FacingCursor(pub usize);

/// Pure decision kernel for victim facing: returns the facing the victim should
/// adopt after a hostile cast, or `None` for same-team (friendly) casts.
///
/// Extracted for unit-testability independent of Bevy ECS.
pub fn victim_face(
    actor_team: Team,
    target_team: Team,
    victim_hex: crate::game::hex::Hex,
    attacker_hex: crate::game::hex::Hex,
) -> Option<crate::game::components::Facing> {
    if actor_team == target_team {
        return None;
    }
    Some(facing_toward(victim_hex, attacker_hex))
}

/// Runs in the Finalize set AFTER `queue_enemy_popup`.
///
/// For each hostile `AbilityUsed` event the victim did not initiate, pushes a
/// `PendingAnim::Face` so the target turns toward the attacker after the popup.
/// Friendly (same-team) casts and self-casts are skipped. AoO is out of scope (v1).
pub fn enqueue_victim_facing(
    log: Res<CombatLog>,
    mut cursor: ResMut<FacingCursor>,
    factions: Query<&Faction>,
    positions: Res<HexPositions>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    let events = &log.0[cursor.0..];
    cursor.0 = log.0.len();

    for event in events {
        let CombatEvent::AbilityUsed { actor, target, .. } = event else {
            continue;
        };

        if actor == target {
            continue;
        }

        // Only hostile (cross-team) casts make the victim turn.
        let (Ok(actor_faction), Ok(target_faction)) = (factions.get(*actor), factions.get(*target))
        else {
            continue;
        };
        if actor_faction.0 == target_faction.0 {
            continue;
        }

        // Both must still have known positions (dead units may have been removed).
        let (Some(victim_hex), Some(attacker_hex)) = (positions.get(target), positions.get(actor))
        else {
            continue;
        };

        anim_queue.0.push_back(PendingAnim::Face {
            unit: *target,
            facing: facing_toward(victim_hex, attacker_hex),
        });
    }
}
