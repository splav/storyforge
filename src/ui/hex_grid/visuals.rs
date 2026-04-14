use super::render::{
    HexBorder, HexCellLink, HexGridOffset, HexHover, HexManaLabel, HexMaterials, HexNameLabel,
    HexHpLabel, TokenMesh,
};
use crate::content::abilities::TargetType;
use crate::game::components::{
    ActiveCombatant, BonusMovement, Combatant, Dead, Faction, HexCell, HexCombatantQ, Mana, Rage, Speed, StartingHexPos,
    Team, UnitToken, Vital,
};
use crate::game::hex::{hex_distance, hex_to_pixel, in_bounds};
use crate::game::pathfinding::reachable_cells;
use crate::game::resources::{
    GameDb, HexPositions, SelectionState, TurnQueue, UiDirty, UiDirtyFlags,
};
use crate::ui::animation::MovePath;
use bevy::prelude::*;
use std::collections::HashSet;

// ── System: UI dirty bridge ───────────────────────────────────────────────────

#[derive(Default)]
pub struct DirtyBridgePrev {
    active: Option<Entity>,
    ability: Option<crate::core::AbilityId>,
    move_mode: bool,
    target: Option<Entity>,
    hover: Option<(i32, i32)>,
    pos_gen: u64,
    initialized: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn ui_dirty_bridge(
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    positions: Res<HexPositions>,
    queue: Res<TurnQueue>,
    hover: Res<HexHover>,
    vitals_q: Query<(), Changed<Vital>>,
    dead_q: Query<(), Added<Dead>>,
    removed_dead: RemovedComponents<Dead>,
    mana_q: Query<(), Changed<Mana>>,
    rage_q: Query<(), Changed<Rage>>,
    mut dirty: ResMut<UiDirty>,
    mut prev: Local<DirtyBridgePrev>,
) {
    if !prev.initialized {
        prev.initialized = true;
        dirty.0 = UiDirtyFlags::all();
        prev.active = active_q.single().ok();
        prev.ability = sel.selected_ability.clone();
        prev.move_mode = sel.move_mode;
        prev.target = sel.selected_target;
        prev.hover = hover.0;
        prev.pos_gen = positions.generation;
        return;
    }

    dirty.0 = UiDirtyFlags::empty();

    if active_q.single().ok() != prev.active {
        prev.active = active_q.single().ok();
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::ABILITY_PANEL
            | UiDirtyFlags::TURN_ORDER
            | UiDirtyFlags::PHASE_HINT
            | UiDirtyFlags::MOVE_BTN;
    }

    if sel.selected_ability != prev.ability {
        prev.ability = sel.selected_ability.clone();
        dirty.0 |= UiDirtyFlags::OVERLAY | UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::PHASE_HINT;
    }

    if sel.move_mode != prev.move_mode {
        prev.move_mode = sel.move_mode;
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::PHASE_HINT
            | UiDirtyFlags::MOVE_BTN
            | UiDirtyFlags::HEX_FILL;
    }

    if sel.selected_target != prev.target {
        prev.target = sel.selected_target;
        dirty.0 |= UiDirtyFlags::HEX_FILL;
    }

    if positions.generation != prev.pos_gen {
        prev.pos_gen = positions.generation;
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::TOKENS;
    }

    if queue.is_changed() {
        dirty.0 |= UiDirtyFlags::TURN_ORDER;
    }

    if !vitals_q.is_empty() {
        dirty.0 |= UiDirtyFlags::LABELS | UiDirtyFlags::TURN_ORDER;
    }

    if !dead_q.is_empty() || !removed_dead.is_empty() {
        dirty.0 |= UiDirtyFlags::HEX_FILL | UiDirtyFlags::TOKENS | UiDirtyFlags::OVERLAY;
    }

    if !mana_q.is_empty() || !rage_q.is_empty() {
        dirty.0 |= UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::LABELS;
    }

    if hover.0 != prev.hover {
        prev.hover = hover.0;
        dirty.0 |= UiDirtyFlags::TOOLTIP;
    }
}

// ── System: Update visuals ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_hex_visuals(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
    mats: Res<HexMaterials>,
    cells: Query<(Entity, &HexCell, &Children)>,
    combatant_q: Query<HexCombatantQ>,
    speed_q: Query<(&Speed, Option<&BonusMovement>)>,
    mut borders: Query<
        (&mut Visibility, &mut MeshMaterial2d<ColorMaterial>),
        (With<HexBorder>, Without<HexNameLabel>, Without<HexHpLabel>, Without<HexManaLabel>),
    >,
    mut name_labels: Query<
        (&HexCellLink, &mut Text2d, &mut Visibility),
        (With<HexNameLabel>, Without<HexBorder>, Without<HexHpLabel>, Without<HexManaLabel>),
    >,
    mut hp_labels: Query<
        (&HexCellLink, &mut Text2d, &mut Visibility),
        (With<HexHpLabel>, Without<HexBorder>, Without<HexNameLabel>, Without<HexManaLabel>),
    >,
    mut mana_labels: Query<
        (&HexCellLink, &mut Text2d, &mut Visibility),
        (With<HexManaLabel>, Without<HexBorder>, Without<HexNameLabel>, Without<HexHpLabel>),
    >,
    mut cell_mats: Query<&mut MeshMaterial2d<ColorMaterial>, (With<HexCell>, Without<HexBorder>)>,
    mut cached_range: Local<HashSet<(i32, i32)>>,
    mut cached_move: Local<HashSet<(i32, i32)>>,
) {
    let flags = dirty.0;
    if !flags.intersects(UiDirtyFlags::OVERLAY | UiDirtyFlags::HEX_FILL | UiDirtyFlags::LABELS) {
        return;
    }

    if flags.contains(UiDirtyFlags::OVERLAY) {
        *cached_range = if !sel.move_mode {
            let info = active_q
                .single()
                .ok()
                .and_then(|e| positions.get(&e))
                .zip(
                    sel.selected_ability
                        .as_ref()
                        .and_then(|id| db.abilities.get(id))
                        .filter(|ab| ab.target_type != TargetType::Myself && ab.range > 0),
                );
            if let Some(((aq, ar), ab)) = info {
                cells
                    .iter()
                    .filter(|(_, hc, _)| hex_distance(aq, ar, hc.q, hc.r) <= ab.range as i32)
                    .map(|(_, hc, _)| (hc.q, hc.r))
                    .collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

        *cached_move = if sel.move_mode {
            if let Some(actor) = active_q.single().ok() {
                if let (Some(actor_pos), Ok((speed, bonus))) =
                    (positions.get(&actor), speed_q.get(actor))
                {
                    let max_steps = bonus.map_or(speed.0, |b| b.0);
                    let enemy_pos: HashSet<(i32, i32)> = positions
                        .iter()
                        .filter(|(&e, _)| {
                            e != actor
                                && combatant_q
                                    .get(e)
                                    .map_or(false, |c| c.faction.0 == Team::Enemy && !c.is_dead)
                        })
                        .map(|(_, &p)| p)
                        .collect();
                    let all_occupied: HashSet<(i32, i32)> = positions
                        .iter()
                        .filter(|(&e, _)| e != actor)
                        .map(|(_, &p)| p)
                        .collect();

                    reachable_cells(
                        actor_pos,
                        max_steps,
                        |q, r| in_bounds(q, r) && !enemy_pos.contains(&(q, r)),
                        |q, r| !all_occupied.contains(&(q, r)),
                    )
                } else {
                    HashSet::new()
                }
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };
    }

    let range_cells = &*cached_range;
    let move_cells = &*cached_move;
    let active = active_q.single().ok();

    for (cell_entity, hex_cell, children) in &cells {
        let occupant = positions.entity_at(hex_cell.q, hex_cell.r);
        let is_active = occupant.is_some_and(|e| active == Some(e));
        let is_target = occupant.is_some_and(|e| sel.selected_target == Some(e));
        let is_in_range = range_cells.contains(&(hex_cell.q, hex_cell.r));
        let is_in_move = move_cells.contains(&(hex_cell.q, hex_cell.r));

        if let Ok(mut mat) = cell_mats.get_mut(cell_entity) {
            mat.0 = match occupant {
                None => {
                    if is_in_move {
                        mats.move_range.clone()
                    } else if is_in_range {
                        mats.in_range.clone()
                    } else {
                        mats.empty.clone()
                    }
                }
                Some(e) => {
                    if let Ok(c) = combatant_q.get(e) {
                        if c.is_dead {
                            mats.dead.clone()
                        } else if c.faction.0 == Team::Player {
                            mats.player.clone()
                        } else {
                            mats.enemy.clone()
                        }
                    } else {
                        mats.empty.clone()
                    }
                }
            };
        }

        for child in children.iter() {
            if let Ok((mut vis, mut bmat)) = borders.get_mut(child) {
                if is_active || is_target || is_in_move || is_in_range {
                    *vis = Visibility::Visible;
                    bmat.0 = if is_active {
                        mats.border_active.clone()
                    } else if is_target {
                        mats.border_target.clone()
                    } else if is_in_move {
                        mats.border_move.clone()
                    } else {
                        mats.border_in_range.clone()
                    };
                } else {
                    *vis = Visibility::Hidden;
                }
            }
        }
    }

    if !flags.contains(UiDirtyFlags::LABELS) {
        return;
    }

    for (link, mut text, mut vis) in &mut name_labels {
        if let Some(c) = super::render::label_occupant(link, &cells, &positions)
            .and_then(|e| combatant_q.get(e).ok())
        {
            let n = c.name.as_str();
            text.0 = if n.chars().count() > 8 {
                n.chars().take(7).collect::<String>() + "."
            } else {
                n.to_string()
            };
            *vis = Visibility::Visible;
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (link, mut text, mut vis) in &mut hp_labels {
        if let Some(c) = super::render::label_occupant(link, &cells, &positions)
            .and_then(|e| combatant_q.get(e).ok())
        {
            text.0 = format!("{}/{}", c.vital.hp, c.vital.max_hp);
            *vis = Visibility::Visible;
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (link, mut text, mut vis) in &mut mana_labels {
        if let Some(c) = super::render::label_occupant(link, &cells, &positions)
            .and_then(|e| combatant_q.get(e).ok())
        {
            if let Some(m) = c.mana {
                text.0 = format!("M:{}/{}", m.current, m.max);
                *vis = Visibility::Visible;
            } else if let Some(r) = c.rage {
                text.0 = format!("R:{}/{}", r.current, r.max);
                *vis = Visibility::Visible;
            } else {
                *vis = Visibility::Hidden;
            }
        } else {
            *vis = Visibility::Hidden;
        }
    }
}

// ── System: Update token positions ────────────────────────────────────────────

/// Syncs UnitToken transforms with HexPositions (when not animating).
/// Also hides tokens of dead units and updates material color.
pub fn update_token_positions(
    dirty: Res<UiDirty>,
    positions: Res<HexPositions>,
    grid_offset: Res<HexGridOffset>,
    mats: Res<HexMaterials>,
    mut tokens: Query<(
        &UnitToken,
        &mut Transform,
        &mut MeshMaterial2d<ColorMaterial>,
        &mut Visibility,
        Has<MovePath>,
    )>,
    combatant_q: Query<(&Faction, Has<Dead>)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TOKENS) {
        return;
    }
    for (token, mut transform, mut mat, mut vis, is_moving) in &mut tokens {
        if is_moving {
            *vis = Visibility::Visible;
            continue;
        }

        let Some(pos) = positions.get(&token.0) else {
            *vis = Visibility::Hidden;
            continue;
        };

        let Ok((faction, is_dead)) = combatant_q.get(token.0) else {
            *vis = Visibility::Hidden;
            continue;
        };

        if is_dead {
            *vis = Visibility::Hidden;
            continue;
        }

        let pixel = hex_to_pixel(pos.0, pos.1) + grid_offset.0;
        transform.translation.x = pixel.x;
        transform.translation.y = pixel.y;

        mat.0 = if faction.0 == Team::Player {
            mats.token_player.clone()
        } else {
            mats.token_enemy.clone()
        };

        *vis = Visibility::Visible;
    }
}
