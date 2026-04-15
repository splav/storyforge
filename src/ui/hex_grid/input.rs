use super::render::{HexGridOffset, HexHover, HexLastClick, HexTooltip, DOUBLE_CLICK_SECS};
use crate::content::abilities::AoEShape;
use crate::game::components::{ActionPoints, ActiveCombatant, BonusMovement, Combatant, Dead, Energy, Faction, Mana, Rage, Speed, StatusEffects, Team, Vital};
use crate::game::hex::{in_bounds, HEX_SIZE};
use crate::game::messages::{MoveUnit, UseAbility};
use crate::game::pathfinding::find_path;
use crate::game::resources::{GameDb, HexPositions, SelectionState, UiDirty, UiDirtyFlags};
use bevy::prelude::*;
use std::collections::HashSet;

// ── Pixel ↔ hex conversion ────────────────────────────────────────────────────

/// World position → nearest hex (col, row). May be out of bounds.
pub fn pixel_to_hex(world_pos: Vec2, grid_offset: Vec2) -> (i32, i32) {
    let pos = world_pos - grid_offset;
    let x = pos.x;
    let y = -pos.y;

    let r_est = (y / (HEX_SIZE * 1.5)).round() as i32;
    let mut best = (0, 0);
    let mut best_dist_sq = f32::MAX;

    for r in (r_est - 1)..=(r_est + 1) {
        let shift = if r & 1 == 0 { 0.5 } else { 0.0 };
        let q = (x / (HEX_SIZE * 3.0_f32.sqrt()) - shift).round() as i32;
        let hx = HEX_SIZE * 3.0_f32.sqrt() * (q as f32 + shift);
        let hy = HEX_SIZE * 1.5 * r as f32;
        let dist_sq = (x - hx).powi(2) + (y - hy).powi(2);
        if dist_sq < best_dist_sq {
            best_dist_sq = dist_sq;
            best = (q, r);
        }
    }

    best
}

// ── System: Hover detection ───────────────────────────────────────────────────

pub fn hex_hover_system(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    grid_offset: Res<HexGridOffset>,
    mut hover: ResMut<HexHover>,
) {
    let Ok(window) = windows.single() else { return };
    let Some(cursor) = window.cursor_position() else {
        hover.0 = None;
        return;
    };
    let Ok((camera, cam_transform)) = camera_q.single() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_transform, cursor) else {
        hover.0 = None;
        return;
    };

    let (q, r) = pixel_to_hex(world_pos, grid_offset.0);
    hover.0 = if in_bounds(q, r) { Some((q, r)) } else { None };
}

// ── System: Tooltip ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn update_hex_tooltip(
    dirty: Res<UiDirty>,
    hover: Res<HexHover>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    combatant_q: Query<(
        &Name,
        &Vital,
        &Faction,
        &StatusEffects,
        Option<&Mana>,
        Option<&Rage>,
        Option<&Energy>,
        Has<Dead>,
    )>,
    mut tooltip_q: Query<(&mut Text, &mut Node, &mut Visibility), With<HexTooltip>>,
    windows: Query<&Window>,
) {
    if !dirty.0.contains(UiDirtyFlags::TOOLTIP) {
        return;
    }
    let Ok((mut text, mut node, mut vis)) = tooltip_q.single_mut() else { return };

    let Some((hq, hr)) = hover.0 else {
        *vis = Visibility::Hidden;
        return;
    };

    let Some(entity) = positions.entity_at(hq, hr) else {
        *vis = Visibility::Hidden;
        return;
    };

    let Ok((name, vital, faction, statuses, mana, rage, energy, is_dead)) = combatant_q.get(entity) else {
        *vis = Visibility::Hidden;
        return;
    };

    let team = if faction.0 == Team::Player { "союзник" } else { "враг" };
    let dead_str = if is_dead { " [мертв]" } else { "" };
    let mut lines = vec![format!("{} ({}){}", name.as_str(), team, dead_str)];
    lines.push(format!("HP: {}/{}  ARM: {}", vital.hp, vital.max_hp, vital.armor));
    if let Some(m) = mana {
        lines.push(format!("Мана: {}/{}", m.current, m.max));
    }
    if let Some(r) = rage {
        lines.push(format!("Ярость: {}/{}", r.current, r.max));
    }
    if let Some(e) = energy {
        lines.push(format!("Энергия: {}/{}", e.current, e.max));
    }
    if !statuses.0.is_empty() {
        let status_strs: Vec<String> = statuses
            .0
            .iter()
            .map(|s| {
                let name = db.statuses.get(&s.id).map(|d| d.name.as_str()).unwrap_or("?");
                format!("{} ({} ход.)", name, s.rounds_remaining)
            })
            .collect();
        lines.push(format!("Статусы: {}", status_strs.join(", ")));
    }

    text.0 = lines.join("\n");
    *vis = Visibility::Visible;

    if let Ok(window) = windows.single() {
        if let Some(cursor) = window.cursor_position() {
            node.left = Val::Px((cursor.x + 16.0).min(window.width() - 200.0));
            node.top = Val::Px((cursor.y + 16.0).min(window.height() - 100.0));
        }
    }
}

// ── System: Click targeting ───────────────────────────────────────────────────

pub fn hex_click_target(
    hover: Res<HexHover>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    positions: Res<HexPositions>,
    db: Res<GameDb>,
    move_query: Query<(&Faction, &ActionPoints, &Speed, Option<&BonusMovement>)>,
    combatant_q2: Query<(&Faction, &Vital), With<Combatant>>,
    mut sel: ResMut<SelectionState>,
    mut last_click: ResMut<HexLastClick>,
    mut use_ability: MessageWriter<UseAbility>,
    mut move_unit: MessageWriter<MoveUnit>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some((hq, hr)) = hover.0 else { return };
    let active = active_q.single().ok();

    let occupant = positions.entity_at(hq, hr);
    let now = time.elapsed_secs_f64();

    if sel.move_mode && occupant.is_none() {
        if try_move(hq, hr, active, &positions, &move_query, &combatant_q2, &mut move_unit) {
            sel.move_mode = false;
        }
        last_click.pos = Some((hq, hr));
        last_click.time = now;
        return;
    }

    // Check if the selected ability is AoE (allows cell targeting).
    let is_aoe = sel
        .selected_ability
        .as_ref()
        .and_then(|id| db.abilities.get(id))
        .is_some_and(|def| def.aoe != AoEShape::None);

    let is_double =
        last_click.pos == Some((hq, hr)) && (now - last_click.time) <= DOUBLE_CLICK_SECS;

    if let Some(entity) = occupant {
        sel.selected_target = Some(entity);
        if is_double {
            if let (Some(actor), Some(ability)) = (active, sel.selected_ability.clone()) {
                let target_pos = positions.get(&entity).unwrap_or((hq, hr));
                use_ability.write(UseAbility { actor, ability, target: entity, target_pos });
            }
        }
    } else if is_double && is_aoe {
        // AoE: double-click on empty cell fires ability at that cell.
        if let (Some(actor), Some(ability)) = (active, sel.selected_ability.clone()) {
            use_ability.write(UseAbility { actor, ability, target: actor, target_pos: (hq, hr) });
        }
    } else if is_double {
        try_move(hq, hr, active, &positions, &move_query, &combatant_q2, &mut move_unit);
    }

    last_click.pos = Some((hq, hr));
    last_click.time = now;
}

/// Tries to path-find and send MoveUnit for the active player to (tq, tr).
/// Returns true if the move was sent.
fn try_move(
    tq: i32,
    tr: i32,
    active: Option<Entity>,
    positions: &HexPositions,
    move_query: &Query<(&Faction, &ActionPoints, &Speed, Option<&BonusMovement>)>,
    combatant_q2: &Query<(&Faction, &Vital), With<Combatant>>,
    move_unit: &mut MessageWriter<MoveUnit>,
) -> bool {
    let Some(actor) = active else { return false };
    let Ok((faction, ap, speed, bonus)) = move_query.get(actor) else { return false };
    if faction.0 != Team::Player || !ap.movement {
        return false;
    }
    let Some(actor_pos) = positions.get(&actor) else { return false };
    let max_steps = bonus.map_or(speed.0, |b| b.0);
    let enemy_pos: HashSet<(i32, i32)> = positions
        .iter()
        .filter(|(&e, _)| {
            e != actor
                && combatant_q2
                    .get(e)
                    .map_or(false, |(f, v)| f.0 == Team::Enemy && v.is_alive())
        })
        .map(|(_, &p)| p)
        .collect();
    let is_passable = |q: i32, r: i32| in_bounds(q, r) && !enemy_pos.contains(&(q, r));
    if let Some(path) = find_path(actor_pos, (tq, tr), is_passable) {
        if path.len() as i32 <= max_steps {
            move_unit.write(MoveUnit { actor, path });
            return true;
        }
    }
    false
}
