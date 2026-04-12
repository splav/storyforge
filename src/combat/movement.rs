use crate::game::components::{ActionPoints, BonusMovement, Speed};
use crate::game::hex::in_bounds;
use crate::game::messages::MoveUnit;
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, HexPositions};
use crate::ui::hex_grid::{HexCell, HexOccupant};
use bevy::prelude::*;

pub fn movement_system(
    mut commands: Commands,
    ctx: Res<CombatContext>,
    mut events: MessageReader<MoveUnit>,
    mut positions: ResMut<HexPositions>,
    mut movers: Query<(&mut ActionPoints, &Speed, Option<&BonusMovement>)>,
    mut cells: Query<(&HexCell, &mut HexOccupant)>,
    mut log: ResMut<CombatLog>,
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

        // Use BonusMovement as speed limit if present, otherwise base Speed.
        let max_steps = bonus.map_or(speed.0, |b| b.0);

        if ev.path.len() as i32 > max_steps {
            continue;
        }

        let dest = *ev.path.last().unwrap();
        if !in_bounds(dest.0, dest.1) {
            continue;
        }

        // Destination must not be occupied by another entity.
        let dest_occupied = positions
            .0
            .iter()
            .any(|(&e, &pos)| e != ev.actor && pos == dest);
        if dest_occupied {
            continue;
        }

        let old_pos = positions.0.get(&ev.actor).copied().unwrap_or((-1, -1));

        // Update resource.
        positions.0.insert(ev.actor, dest);

        // Update cell occupants.
        for (cell, mut occ) in &mut cells {
            if cell.q == old_pos.0 && cell.r == old_pos.1 && occ.0 == Some(ev.actor) {
                occ.0 = None;
            }
        }
        for (cell, mut occ) in &mut cells {
            if cell.q == dest.0 && cell.r == dest.1 {
                occ.0 = Some(ev.actor);
            }
        }

        ap.movement = false;

        // Remove BonusMovement component after use.
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
