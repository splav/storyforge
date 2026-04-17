#![allow(clippy::too_many_arguments)]
use crate::content::content_view::ActiveContent;
use crate::game::components::{ActionPoints, ActiveCombatant, BonusMovement, Speed, StatusEffects, UnitToken};
use crate::game::hex::{in_bounds, Hex, LAYOUT};
use crate::game::messages::MoveUnit;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::HexPositions;
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::HexGridOffset;
use bevy::prelude::*;


pub fn movement_system(
    mut commands: Commands,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut events: MessageReader<MoveUnit>,
    mut positions: ResMut<HexPositions>,
    mut movers: Query<(&mut ActionPoints, &Speed, Option<&BonusMovement>, Option<&StatusEffects>)>,
    content: Res<ActiveContent>,
    mut log: ResMut<CombatLog>,
    tokens: Query<(Entity, &UnitToken)>,
    grid_offset: Res<HexGridOffset>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        if active != Some(ev.actor) {
            continue;
        }
        if ev.path.is_empty() {
            continue;
        }

        let Ok((mut ap, speed, bonus, statuses)) = movers.get_mut(ev.actor) else {
            continue;
        };
        if !ap.movement {
            continue;
        }

        let speed_mod: i32 = statuses.map_or(0, |se| {
            se.0.iter()
                .filter_map(|s| content.statuses.get(&s.id))
                .map(|d| d.speed_bonus)
                .sum()
        });
        // .max(0) — negative status debuffs can push modified speed below zero; clamp there.
        // A base speed of 0 legitimately means "immobile" and must survive the clamp.
        let max_steps = bonus.map_or((speed.0 + speed_mod).max(0), |b| b.0);
        if ev.path.len() as i32 > max_steps {
            continue;
        }

        let dest = *ev.path.last().unwrap();
        if !in_bounds(dest) {
            continue;
        }

        let dest_occupied = positions
            .entity_at(dest)
            .is_some_and(|e| e != ev.actor);
        if dest_occupied {
            continue;
        }

        let old_pos = positions.get(&ev.actor).unwrap_or(Hex::ZERO);

        // Build pixel waypoints for animation: start from old position, then path steps.
        let offset = grid_offset.0;
        let mut waypoints = vec![LAYOUT.hex_to_world_pos(old_pos) + offset];
        for &h in &ev.path {
            waypoints.push(LAYOUT.hex_to_world_pos(h) + offset);
        }

        // Find the token entity for this actor.
        if let Some((token_entity, _)) = tokens.iter().find(|(_, t)| t.0 == ev.actor) {
            anim_queue.0.push_back(PendingAnim::Movement {
                token: token_entity,
                waypoints,
            });
        }

        positions.insert(ev.actor, dest);

        ap.movement = false;

        if bonus.is_some() {
            commands.entity(ev.actor).remove::<BonusMovement>();
        }

        log.push(CombatEvent::UnitMoved {
            actor: ev.actor,
            from: old_pos,
            to: dest,
        });
    }
}
