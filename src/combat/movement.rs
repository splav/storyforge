use crate::game::components::{ActionPoints, BonusMovement, Speed, UnitToken};
use crate::game::hex::in_bounds;
use crate::game::messages::MoveUnit;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatContext, HexPositions};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::HexGridOffset;
use bevy::prelude::*;

/// Hex-to-pixel for animation waypoints (duplicated from hex_grid for layering).
fn hex_to_pixel(q: i32, r: i32) -> Vec2 {
    const HEX_SIZE: f32 = 34.0;
    let shift = if r & 1 == 0 { 0.5 } else { 0.0 };
    let x = HEX_SIZE * 3.0_f32.sqrt() * (q as f32 + shift);
    let y = HEX_SIZE * 1.5 * r as f32;
    Vec2::new(x, -y)
}

pub fn movement_system(
    mut commands: Commands,
    ctx: Res<CombatContext>,
    mut events: MessageReader<MoveUnit>,
    mut positions: ResMut<HexPositions>,
    mut movers: Query<(&mut ActionPoints, &Speed, Option<&BonusMovement>)>,
    mut log: ResMut<CombatLog>,
    tokens: Query<(Entity, &UnitToken)>,
    grid_offset: Res<HexGridOffset>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    for ev in events.read() {
        if ctx.active != Some(ev.actor) {
            continue;
        }
        if ev.path.is_empty() {
            continue;
        }

        let Ok((mut ap, speed, bonus)) = movers.get_mut(ev.actor) else {
            continue;
        };
        if !ap.movement {
            continue;
        }

        let max_steps = bonus.map_or(speed.0, |b| b.0);
        if ev.path.len() as i32 > max_steps {
            continue;
        }

        let dest = *ev.path.last().unwrap();
        if !in_bounds(dest.0, dest.1) {
            continue;
        }

        let dest_occupied = positions
            .entity_at(dest.0, dest.1)
            .is_some_and(|e| e != ev.actor);
        if dest_occupied {
            continue;
        }

        let old_pos = positions.get(&ev.actor).unwrap_or((-1, -1));

        // Build pixel waypoints for animation: start from old position, then path steps.
        let offset = grid_offset.0;
        let mut waypoints = vec![hex_to_pixel(old_pos.0, old_pos.1) + offset];
        for &(q, r) in &ev.path {
            waypoints.push(hex_to_pixel(q, r) + offset);
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
