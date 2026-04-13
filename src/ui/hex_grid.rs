use crate::content::abilities::TargetType;
use crate::game::components::{ActionPoints, BonusMovement, Combatant, Dead, Faction, HexCell, HexCombatantQ, Mana, Rage, Speed, StartingHexPos, StatusEffects, Team, UnitToken, Vital};
use crate::ui::animation::MovePath;
use crate::game::hex::{hex_distance, in_bounds, row_cols, GRID_COLS, GRID_ROWS};
use crate::game::messages::{MoveUnit, UseAbility};
use crate::game::pathfinding::{find_path, reachable_cells};
use crate::game::resources::{CombatContext, GameDb, HexPositions, SelectionState, TurnQueue, UiDirty, UiDirtyFlags};
use bevy::prelude::*;
use bevy::sprite::Anchor;
use std::collections::HashSet;

// ── Constants ────────────────────────────────────────────────────────────────

const HEX_SIZE: f32 = 34.0;
/// Y offset to push grid up so bottom UI has room.
const GRID_Y_OFFSET: f32 = 40.0;

// ── Colors ───────────────────────────────────────────────────────────────────

const CLR_EMPTY: Color = Color::srgb(0.12, 0.12, 0.14);
const CLR_PLAYER: Color = Color::srgb(0.10, 0.14, 0.22);
const CLR_ENEMY: Color = Color::srgb(0.22, 0.10, 0.10);
const CLR_DEAD: Color = Color::srgb(0.15, 0.15, 0.15);
const CLR_BORDER_ACTIVE: Color = Color::srgb(0.85, 0.75, 0.20);
const CLR_BORDER_TARGET: Color = Color::srgb(0.85, 0.20, 0.20);
const CLR_IN_RANGE: Color = Color::srgb(0.10, 0.20, 0.18);
const CLR_BORDER_IN_RANGE: Color = Color::srgb(0.20, 0.60, 0.52);
const CLR_MOVE_RANGE: Color = Color::srgb(0.12, 0.20, 0.10);
const CLR_BORDER_MOVE: Color = Color::srgb(0.30, 0.65, 0.25);

// ── Hex math ─────────────────────────────────────────────────────────────────

/// Pointy-top hex, even rows shift right 0.5 → odd rows are longer on both sides.
fn hex_to_pixel(q: i32, r: i32) -> Vec2 {
    let shift = if r & 1 == 0 { 0.5 } else { 0.0 };
    let x = HEX_SIZE * 3.0_f32.sqrt() * (q as f32 + shift);
    let y = HEX_SIZE * 1.5 * r as f32;
    Vec2::new(x, -y)
}

/// Grid center: odd rows span 0..(GRID_COLS-1)*spacing, both row types centered the same.
fn grid_center() -> Vec2 {
    let cx = (GRID_COLS - 1) as f32 * 0.5 * HEX_SIZE * 3.0_f32.sqrt();
    let cy = (GRID_ROWS - 1) as f32 * 0.5 * HEX_SIZE * 1.5;
    Vec2::new(cx, -cy)
}

/// World position → nearest hex (col, row). May be out of bounds.
fn pixel_to_hex(world_pos: Vec2, grid_offset: Vec2) -> (i32, i32) {
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


// ── Mesh ─────────────────────────────────────────────────────────────────────
// Using Bevy's built-in RegularPolygon primitive for hexagon meshes.

// ── Components ───────────────────────────────────────────────────────────────

#[derive(Component)]
pub struct HexBorder;

/// Links a standalone label entity to its hex cell entity.
#[derive(Component)]
pub struct HexCellLink(pub Entity);

#[derive(Component)]
pub struct HexNameLabel;

#[derive(Component)]
pub struct HexHpLabel;

#[derive(Component)]
pub struct HexManaLabel;

#[derive(Component)]
pub struct HexTooltip;

// ── Resources ────────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct HexHover(pub Option<(i32, i32)>);

const DOUBLE_CLICK_SECS: f64 = 0.35;

/// Tracks the last click for double-click detection.
#[derive(Resource, Default)]
pub struct HexLastClick {
    pub pos: Option<(i32, i32)>,
    pub time: f64,
}

/// Cached material handles used by hex cells.
#[derive(Resource)]
pub struct HexMaterials {
    empty: Handle<ColorMaterial>,
    player: Handle<ColorMaterial>,
    enemy: Handle<ColorMaterial>,
    dead: Handle<ColorMaterial>,
    in_range: Handle<ColorMaterial>,
    move_range: Handle<ColorMaterial>,
    border_active: Handle<ColorMaterial>,
    border_target: Handle<ColorMaterial>,
    border_in_range: Handle<ColorMaterial>,
    border_move: Handle<ColorMaterial>,
    token_player: Handle<ColorMaterial>,
    token_enemy: Handle<ColorMaterial>,
    token_dead: Handle<ColorMaterial>,
}

/// Cached token circle mesh handle.
#[derive(Resource)]
pub struct TokenMesh(pub Handle<Mesh>);

/// Grid parent transform offset, cached once at setup.
#[derive(Resource)]
pub struct HexGridOffset(pub Vec2);

// ── System 1: Setup ──────────────────────────────────────────────────────────

pub fn setup_hex_grid(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    asset_server: Res<AssetServer>,
) {
    let hex_mesh = meshes.add(RegularPolygon::new(HEX_SIZE * 0.97, 6));
    let border_mesh = meshes.add(RegularPolygon::new(HEX_SIZE * 1.06, 6));
    let token_mesh = meshes.add(Circle::new(HEX_SIZE * 0.75));

    let mats = HexMaterials {
        empty: materials.add(ColorMaterial::from_color(CLR_EMPTY)),
        player: materials.add(ColorMaterial::from_color(CLR_PLAYER)),
        enemy: materials.add(ColorMaterial::from_color(CLR_ENEMY)),
        dead: materials.add(ColorMaterial::from_color(CLR_DEAD)),
        in_range: materials.add(ColorMaterial::from_color(CLR_IN_RANGE)),
        move_range: materials.add(ColorMaterial::from_color(CLR_MOVE_RANGE)),
        border_active: materials.add(ColorMaterial::from_color(CLR_BORDER_ACTIVE)),
        border_target: materials.add(ColorMaterial::from_color(CLR_BORDER_TARGET)),
        border_in_range: materials.add(ColorMaterial::from_color(CLR_BORDER_IN_RANGE)),
        border_move: materials.add(ColorMaterial::from_color(CLR_BORDER_MOVE)),
        token_player: materials.add(ColorMaterial::from_color(Color::srgb(0.12, 0.22, 0.45))),
        token_enemy: materials.add(ColorMaterial::from_color(Color::srgb(0.45, 0.10, 0.08))),
        token_dead: materials.add(ColorMaterial::from_color(Color::srgb(0.3, 0.3, 0.3))),
    };

    let center = grid_center();
    let offset = Vec2::new(-center.x, -center.y + GRID_Y_OFFSET);
    commands.insert_resource(HexGridOffset(offset));

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    for r in 0..GRID_ROWS {
        for q in 0..row_cols(r) {
            let pixel = hex_to_pixel(q, r) + offset;

            let cell_id = commands
                .spawn((
                    HexCell { q, r },
                    Mesh2d(hex_mesh.clone()),
                    MeshMaterial2d(mats.empty.clone()),
                    Transform::from_xyz(pixel.x, pixel.y, 0.1),
                ))
                .with_children(|parent| {
                    // Border (behind the fill).
                    parent.spawn((
                        HexBorder,
                        Mesh2d(border_mesh.clone()),
                        MeshMaterial2d(mats.border_active.clone()),
                        Transform::from_xyz(0.0, 0.0, -0.05),
                        Visibility::Hidden,
                    ));
                })
                .id();

            // Labels — independent entities in world space (no rotation).
            commands.spawn((
                HexCellLink(cell_id),
                HexNameLabel,
                Text2d::new(""),
                TextFont {
                    font: font.clone(),
                    font_size: 11.0,
                    ..default()
                },
                TextLayout::new_with_justify(Justify::Center),
                TextColor(Color::WHITE),
                Anchor::CENTER,
                Transform::from_xyz(pixel.x, pixel.y + 10.0, 0.2),
                Visibility::Hidden,
            ));
            commands.spawn((
                HexCellLink(cell_id),
                HexHpLabel,
                Text2d::new(""),
                TextFont {
                    font: font.clone(),
                    font_size: 10.0,
                    ..default()
                },
                TextLayout::new_with_justify(Justify::Center),
                TextColor(Color::srgb(0.6, 0.9, 0.6)),
                Anchor::CENTER,
                Transform::from_xyz(pixel.x, pixel.y - 4.0, 0.2),
                Visibility::Hidden,
            ));
            commands.spawn((
                HexCellLink(cell_id),
                HexManaLabel,
                Text2d::new(""),
                TextFont {
                    font: font.clone(),
                    font_size: 9.0,
                    ..default()
                },
                TextLayout::new_with_justify(Justify::Center),
                TextColor(Color::srgb(0.85, 0.90, 1.0)),
                Anchor::CENTER,
                Transform::from_xyz(pixel.x, pixel.y - 16.0, 0.2),
                Visibility::Hidden,
            ));
        }
    }

    // Tooltip UI node (screen-space, hidden by default).
    commands.spawn((
        HexTooltip,
        Text::new(""),
        TextFont {
            font,
            font_size: 12.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.08, 0.08, 0.10, 0.92)),
        Visibility::Hidden,
        // High z-index so it's on top of everything.
        ZIndex(100),
    ));

    commands.insert_resource(mats);
    commands.insert_resource(TokenMesh(token_mesh));
}

// ── System 2: Assign positions ───────────────────────────────────────────────

pub fn assign_hex_positions(
    mut commands: Commands,
    mut positions: ResMut<HexPositions>,
    combatants: Query<(Entity, &StartingHexPos, &Faction), With<Combatant>>,
    grid_offset: Res<HexGridOffset>,
    mats: Res<HexMaterials>,
    token_mesh: Res<TokenMesh>,
) {
    positions.clear();
    for (entity, hex_pos, faction) in &combatants {
        positions.insert(entity, (hex_pos.0, hex_pos.1));
        commands.entity(entity).remove::<StartingHexPos>();

        let pixel = hex_to_pixel(hex_pos.0, hex_pos.1) + grid_offset.0;
        let mat = if faction.0 == Team::Player {
            mats.token_player.clone()
        } else {
            mats.token_enemy.clone()
        };
        commands.spawn((
            UnitToken(entity),
            Mesh2d(token_mesh.0.clone()),
            MeshMaterial2d(mat),
            Transform::from_xyz(pixel.x, pixel.y, 0.15),
        ));
    }
}

// ── System: UI dirty bridge ──────────────────────────────────────────────────

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
    ctx: Res<CombatContext>,
    sel: Res<SelectionState>,
    positions: Res<HexPositions>,
    queue: Res<TurnQueue>,
    hover: Res<HexHover>,
    vitals_q: Query<(), Changed<Vital>>,
    dead_q: Query<(), Added<Dead>>,
    mana_q: Query<(), Changed<Mana>>,
    rage_q: Query<(), Changed<Rage>>,
    mut dirty: ResMut<UiDirty>,
    mut prev: Local<DirtyBridgePrev>,
) {
    if !prev.initialized {
        prev.initialized = true;
        dirty.0 = UiDirtyFlags::all();
        prev.active = ctx.active;
        prev.ability = sel.selected_ability.clone();
        prev.move_mode = sel.move_mode;
        prev.target = sel.selected_target;
        prev.hover = hover.0;
        prev.pos_gen = positions.generation;
        return;
    }

    dirty.0 = UiDirtyFlags::empty();

    if ctx.active != prev.active {
        prev.active = ctx.active;
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

    if !dead_q.is_empty() {
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

// ── System 3: Update visuals ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_hex_visuals(
    dirty: Res<UiDirty>,
    ctx: Res<CombatContext>,
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

    // Recompute overlay only when OVERLAY flag is set.
    if flags.contains(UiDirtyFlags::OVERLAY) {
        // Compute ability range cells.
        *cached_range = if !sel.move_mode {
            let info = ctx.active
                .and_then(|e| positions.get(&e))
                .zip(
                    sel.selected_ability.as_ref()
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

        // Compute movement-reachable cells.
        *cached_move = if sel.move_mode {
            if let Some(actor) = ctx.active {
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

    // Update cell fill + border.
    for (cell_entity, hex_cell, children) in &cells {
        let occupant = positions.entity_at(hex_cell.q, hex_cell.r);
        let is_active = occupant.is_some_and(|e| ctx.active == Some(e));
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

    // Update standalone labels (only when LABELS dirty).
    if !flags.contains(UiDirtyFlags::LABELS) {
        return;
    }
    for (link, mut text, mut vis) in &mut name_labels {
        let occupant = cells.get(link.0).ok().and_then(|(_, hc, _)| positions.entity_at(hc.q, hc.r));
        if let Some(e) = occupant {
            if let Ok(c) = combatant_q.get(e) {
                let n = c.name.as_str();
                text.0 = if n.chars().count() > 8 {
                    n.chars().take(7).collect::<String>() + "."
                } else {
                    n.to_string()
                };
                *vis = Visibility::Visible;
                continue;
            }
        }
        *vis = Visibility::Hidden;
    }

    for (link, mut text, mut vis) in &mut hp_labels {
        let occupant = cells.get(link.0).ok().and_then(|(_, hc, _)| positions.entity_at(hc.q, hc.r));
        if let Some(e) = occupant {
            if let Ok(c) = combatant_q.get(e) {
                text.0 = format!("{}/{}", c.vital.hp, c.vital.max_hp);
                *vis = Visibility::Visible;
                continue;
            }
        }
        *vis = Visibility::Hidden;
    }

    for (link, mut text, mut vis) in &mut mana_labels {
        let occupant = cells.get(link.0).ok().and_then(|(_, hc, _)| positions.entity_at(hc.q, hc.r));
        if let Some(e) = occupant {
            if let Ok(c) = combatant_q.get(e) {
                if let Some(m) = c.mana {
                    text.0 = format!("M:{}/{}", m.current, m.max);
                    *vis = Visibility::Visible;
                } else if let Some(r) = c.rage {
                    text.0 = format!("R:{}/{}", r.current, r.max);
                    *vis = Visibility::Visible;
                } else {
                    *vis = Visibility::Hidden;
                }
                continue;
            }
        }
        *vis = Visibility::Hidden;
    }
}

// ── System 4: Hover detection ────────────────────────────────────────────────

pub fn hex_hover_system(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    grid_offset: Res<HexGridOffset>,
    mut hover: ResMut<HexHover>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        hover.0 = None;
        return;
    };
    let Ok((camera, cam_transform)) = camera_q.single() else {
        return;
    };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_transform, cursor) else {
        hover.0 = None;
        return;
    };

    let (q, r) = pixel_to_hex(world_pos, grid_offset.0);
    hover.0 = if in_bounds(q, r) { Some((q, r)) } else { None };
}

// ── System 5: Tooltip ────────────────────────────────────────────────────────

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
        Has<Dead>,
    )>,
    mut tooltip_q: Query<(&mut Text, &mut Node, &mut Visibility), With<HexTooltip>>,
    windows: Query<&Window>,
) {
    if !dirty.0.contains(UiDirtyFlags::TOOLTIP) {
        return;
    }
    let Ok((mut text, mut node, mut vis)) = tooltip_q.single_mut() else {
        return;
    };

    let Some((hq, hr)) = hover.0 else {
        *vis = Visibility::Hidden;
        return;
    };

    // Find occupant of hovered cell.
    let occupant = positions.entity_at(hq, hr);

    let Some(entity) = occupant else {
        *vis = Visibility::Hidden;
        return;
    };

    let Ok((name, vital, faction, statuses, mana, rage, is_dead)) = combatant_q.get(entity) else {
        *vis = Visibility::Hidden;
        return;
    };

    // Build tooltip text.
    let team = if faction.0 == Team::Player {
        "союзник"
    } else {
        "враг"
    };
    let dead_str = if is_dead { " [мертв]" } else { "" };
    let mut lines = vec![format!("{} ({}){}", name.as_str(), team, dead_str)];
    lines.push(format!(
        "HP: {}/{}  ARM: {}",
        vital.hp, vital.max_hp, vital.armor
    ));
    if let Some(m) = mana {
        lines.push(format!("Мана: {}/{}", m.current, m.max));
    }
    if let Some(r) = rage {
        lines.push(format!("Ярость: {}/{}", r.current, r.max));
    }
    if !statuses.0.is_empty() {
        let status_strs: Vec<String> = statuses
            .0
            .iter()
            .map(|s| {
                let name = db
                    .statuses
                    .get(&s.id)
                    .map(|d| d.name.as_str())
                    .unwrap_or("?");
                format!("{} ({} ход.)", name, s.rounds_remaining)
            })
            .collect();
        lines.push(format!("Статусы: {}", status_strs.join(", ")));
    }

    text.0 = lines.join("\n");
    *vis = Visibility::Visible;

    // Position tooltip near cursor.
    if let Ok(window) = windows.single() {
        if let Some(cursor) = window.cursor_position() {
            node.left = Val::Px((cursor.x + 16.0).min(window.width() - 200.0));
            node.top = Val::Px((cursor.y + 16.0).min(window.height() - 100.0));
        }
    }
}

// ── System 6: Click targeting ────────────────────────────────────────────────

pub fn hex_click_target(
    hover: Res<HexHover>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    ctx: Res<CombatContext>,
    positions: Res<HexPositions>,
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

    let occupant = positions.entity_at(hq, hr);
    let now = time.elapsed_secs_f64();

    /// Try to move the active player actor to (tq, tr). Returns true if MoveUnit was sent.
    fn try_move(
        tq: i32,
        tr: i32,
        ctx: &CombatContext,
        positions: &HexPositions,
        move_query: &Query<(&Faction, &ActionPoints, &Speed, Option<&BonusMovement>)>,
        combatant_q2: &Query<(&Faction, &Vital), With<Combatant>>,
        move_unit: &mut MessageWriter<MoveUnit>,
    ) -> bool {
        let Some(actor) = ctx.active else { return false };
        let Ok((faction, ap, speed, bonus)) = move_query.get(actor) else {
            return false;
        };
        if faction.0 != Team::Player || !ap.movement {
            return false;
        }
        let Some(actor_pos) = positions.get(&actor) else {
            return false;
        };
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

    // Move mode: clicking an empty cell sends MoveUnit.
    if sel.move_mode && occupant.is_none() {
        if try_move(hq, hr, &ctx, &positions, &move_query, &combatant_q2, &mut move_unit) {
            sel.move_mode = false;
        }
        last_click.pos = Some((hq, hr));
        last_click.time = now;
        return;
    }

    let is_double = last_click.pos == Some((hq, hr))
        && (now - last_click.time) <= DOUBLE_CLICK_SECS;

    if let Some(entity) = occupant {
        sel.selected_target = Some(entity);
        if is_double {
            if let (Some(actor), Some(ability)) = (ctx.active, sel.selected_ability.clone()) {
                use_ability.write(UseAbility {
                    actor,
                    ability,
                    target: entity,
                });
            }
        }
    } else if is_double {
        // Double-click empty cell → move there (without entering move mode).
        try_move(hq, hr, &ctx, &positions, &move_query, &combatant_q2, &mut move_unit);
    }

    last_click.pos = Some((hq, hr));
    last_click.time = now;
}

// ── System 7: Update token positions ────────────────────────────────────────

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
        // Skip tokens currently animating.
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
