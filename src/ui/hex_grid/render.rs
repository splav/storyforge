#![allow(clippy::too_many_arguments)]
use crate::game::components::{Combatant, Faction, StartingHexPos, Team, UnitToken, VictoryTarget};
use crate::game::hex::{hex_from_offset, row_cols, Hex, GRID_COLS, GRID_ROWS, HEX_SIZE, LAYOUT};
use crate::game::hex_map::HexMap;
use crate::game::resources::HexPositions;
use bevy::prelude::*;
use bevy::sprite::Anchor;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Y offset to push grid down so top log has room.
pub const GRID_Y_OFFSET: f32 = -30.0;

// ── Colors ────────────────────────────────────────────────────────────────────

pub const CLR_EMPTY: Color = Color::srgba(0.12, 0.12, 0.14, 0.45);
pub const CLR_PLAYER: Color = Color::srgba(0.10, 0.14, 0.22, 0.55);
pub const CLR_ENEMY: Color = Color::srgba(0.22, 0.10, 0.10, 0.55);
pub const CLR_DEAD: Color = Color::srgba(0.15, 0.15, 0.15, 0.55);
pub const CLR_BORDER_ACTIVE: Color = Color::srgb(0.85, 0.75, 0.20);
pub const CLR_BORDER_TARGET: Color = Color::srgb(0.85, 0.20, 0.20);
pub const CLR_IN_RANGE: Color = Color::srgba(0.10, 0.20, 0.18, 0.35);
pub const CLR_BORDER_IN_RANGE: Color = Color::srgb(0.20, 0.60, 0.52);
/// Cells within max range but below min range (disadvantage zone).
pub const CLR_IN_RANGE_DIM: Color = Color::srgba(0.11, 0.14, 0.14, 0.3);
pub const CLR_BORDER_IN_RANGE_DIM: Color = Color::srgb(0.18, 0.35, 0.30);
pub const CLR_MOVE_RANGE: Color = Color::srgba(0.12, 0.20, 0.10, 0.35);
pub const CLR_BORDER_MOVE: Color = Color::srgb(0.30, 0.65, 0.25);
/// AoE blast zone preview.
pub const CLR_AOE_PREVIEW: Color = Color::srgba(0.22, 0.12, 0.06, 0.4);
pub const CLR_BORDER_AOE: Color = Color::srgb(0.70, 0.35, 0.10);

// ── Components ────────────────────────────────────────────────────────────────

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

// ── Resources ─────────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct HexHover(pub Option<Hex>);

pub const DOUBLE_CLICK_SECS: f64 = 0.35;

/// Tracks the last click for double-click detection.
#[derive(Resource, Default)]
pub struct HexLastClick {
    pub pos: Option<Hex>,
    pub time: f64,
}

/// Cached material handles used by hex cells.
#[derive(Resource)]
pub struct HexMaterials {
    pub empty: Handle<ColorMaterial>,
    pub player: Handle<ColorMaterial>,
    pub enemy: Handle<ColorMaterial>,
    pub dead: Handle<ColorMaterial>,
    pub in_range: Handle<ColorMaterial>,
    pub in_range_dim: Handle<ColorMaterial>,
    pub move_range: Handle<ColorMaterial>,
    pub border_active: Handle<ColorMaterial>,
    pub border_target: Handle<ColorMaterial>,
    pub border_in_range: Handle<ColorMaterial>,
    pub border_in_range_dim: Handle<ColorMaterial>,
    pub border_move: Handle<ColorMaterial>,
    pub aoe_preview: Handle<ColorMaterial>,
    pub border_aoe: Handle<ColorMaterial>,
    pub token_player: Handle<ColorMaterial>,
    pub token_enemy: Handle<ColorMaterial>,
    pub token_dead: Handle<ColorMaterial>,
}

/// Cached token circle mesh handle.
#[derive(Resource)]
pub struct TokenMesh {
    pub token: Handle<Mesh>,
    /// Slightly larger circle, drawn behind the token to form a colored ring.
    pub ring: Handle<Mesh>,
}

/// Grid parent transform offset, cached once at setup.
#[derive(Resource)]
pub struct HexGridOffset(pub Vec2);

// ── Label helpers ─────────────────────────────────────────────────────────────

/// Spawns a single world-space text label linked to a hex cell entity.
pub fn spawn_hex_label<M: Component>(
    commands: &mut Commands,
    cell_id: Entity,
    marker: M,
    pixel: Vec2,
    font: Handle<Font>,
    font_size: f32,
    color: Color,
    y_offset: f32,
) {
    commands.spawn((
        HexCellLink(cell_id),
        marker,
        Text2d::new(""),
        TextFont { font, font_size, ..default() },
        TextLayout::new_with_justify(Justify::Center),
        TextColor(color),
        Anchor::CENTER,
        Transform::from_xyz(pixel.x, pixel.y + y_offset, 0.2),
        Visibility::Hidden,
    ));
}

/// Resolves the entity occupying the hex cell that a label is linked to.
///
/// Living unit takes priority. If no living unit is at the hex, falls back to
/// the first corpse — so labels still render the dead unit's name/HP for the
/// gray-filled corpse tile.
pub fn label_occupant(
    link: &HexCellLink,
    cells: &Query<(Entity, &Hex, &Children)>,
    map: &HexMap,
) -> Option<Entity> {
    cells.get(link.0).ok().and_then(|(_, &hex, _)| map.any_at(hex))
}

// ── Grid math ─────────────────────────────────────────────────────────────────

/// Grid center: odd rows span 0..(GRID_COLS-1)*spacing, both row types centered the same.
pub fn grid_center() -> Vec2 {
    let cx = (GRID_COLS - 1) as f32 * 0.5 * HEX_SIZE * 3.0_f32.sqrt();
    let cy = (GRID_ROWS - 1) as f32 * 0.5 * HEX_SIZE * 1.5;
    Vec2::new(cx, -cy)
}

// ── System: Setup ─────────────────────────────────────────────────────────────

pub fn setup_hex_grid(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    asset_server: Res<AssetServer>,
) {
    let hex_mesh = meshes.add(RegularPolygon::new(HEX_SIZE * 0.97, 6));
    let border_mesh = meshes.add(RegularPolygon::new(HEX_SIZE * 1.06, 6));
    let token_mesh = meshes.add(Circle::new(HEX_SIZE * 0.75));
    let target_ring_mesh = meshes.add(Circle::new(HEX_SIZE * 0.88));

    let mats = HexMaterials {
        empty: materials.add(ColorMaterial::from_color(CLR_EMPTY)),
        player: materials.add(ColorMaterial::from_color(CLR_PLAYER)),
        enemy: materials.add(ColorMaterial::from_color(CLR_ENEMY)),
        dead: materials.add(ColorMaterial::from_color(CLR_DEAD)),
        in_range: materials.add(ColorMaterial::from_color(CLR_IN_RANGE)),
        in_range_dim: materials.add(ColorMaterial::from_color(CLR_IN_RANGE_DIM)),
        move_range: materials.add(ColorMaterial::from_color(CLR_MOVE_RANGE)),
        border_active: materials.add(ColorMaterial::from_color(CLR_BORDER_ACTIVE)),
        border_target: materials.add(ColorMaterial::from_color(CLR_BORDER_TARGET)),
        border_in_range: materials.add(ColorMaterial::from_color(CLR_BORDER_IN_RANGE)),
        border_in_range_dim: materials.add(ColorMaterial::from_color(CLR_BORDER_IN_RANGE_DIM)),
        border_move: materials.add(ColorMaterial::from_color(CLR_BORDER_MOVE)),
        aoe_preview: materials.add(ColorMaterial::from_color(CLR_AOE_PREVIEW)),
        border_aoe: materials.add(ColorMaterial::from_color(CLR_BORDER_AOE)),
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
            let hex = hex_from_offset(q, r);
            let pixel = LAYOUT.hex_to_world_pos(hex) + offset;

            let cell_id = commands
                .spawn((
                    hex,
                    Mesh2d(hex_mesh.clone()),
                    MeshMaterial2d(mats.empty.clone()),
                    Transform::from_xyz(pixel.x, pixel.y, 0.1),
                ))
                .with_children(|parent| {
                    parent.spawn((
                        HexBorder,
                        Mesh2d(border_mesh.clone()),
                        MeshMaterial2d(mats.border_active.clone()),
                        Transform::from_xyz(0.0, 0.0, -0.05),
                        Visibility::Hidden,
                    ));
                })
                .id();

            spawn_hex_label(&mut commands, cell_id, HexNameLabel, pixel, font.clone(), 11.0, Color::WHITE,                      10.0);
            spawn_hex_label(&mut commands, cell_id, HexHpLabel,   pixel, font.clone(), 10.0, Color::srgb(0.6,  0.9,  0.6),  -4.0);
            spawn_hex_label(&mut commands, cell_id, HexManaLabel, pixel, font.clone(),  9.0, Color::srgb(0.85, 0.90, 1.0), -16.0);
        }
    }

    // Tooltip UI node (screen-space, hidden by default).
    commands.spawn((
        HexTooltip,
        Text::new(""),
        TextFont { font, font_size: 12.0, ..default() },
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
        ZIndex(100),
    ));

    commands.insert_resource(mats);
    commands.insert_resource(TokenMesh {
        token: token_mesh,
        ring: target_ring_mesh,
    });
}

// ── System: Assign positions ──────────────────────────────────────────────────

/// Assigns hex positions and spawns visual tokens for combatants that still
/// have a `StartingHexPos` marker. The marker is removed after assignment,
/// so on subsequent rounds this is a no-op.
pub fn assign_hex_positions(
    mut commands: Commands,
    mut positions: ResMut<HexPositions>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    combatants: Query<(Entity, &StartingHexPos, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    grid_offset: Res<HexGridOffset>,
    mats: Res<HexMaterials>,
    token_mesh: Res<TokenMesh>,
) {
    if combatants.is_empty() {
        return;
    }
    positions.clear();
    for (entity, hex_pos, faction, target) in &combatants {
        positions.insert(entity, hex_pos.0);
        commands.entity(entity).remove::<StartingHexPos>();

        let pixel = LAYOUT.hex_to_world_pos(hex_pos.0) + grid_offset.0;
        let mat = if faction.0 == Team::Player {
            mats.token_player.clone()
        } else {
            mats.token_enemy.clone()
        };
        commands
            .spawn((
                UnitToken(entity),
                Mesh2d(token_mesh.token.clone()),
                MeshMaterial2d(mat),
                Transform::from_xyz(pixel.x, pixel.y, 0.15),
            ))
            .with_children(|parent| {
                if let Some(target) = target {
                    let [r, g, b] = target.marker_color;
                    let ring_mat = materials.add(ColorMaterial::from_color(Color::srgb(r, g, b)));
                    parent.spawn((
                        Mesh2d(token_mesh.ring.clone()),
                        MeshMaterial2d(ring_mat),
                        // Behind the token (negative z relative to parent).
                        Transform::from_xyz(0.0, 0.0, -0.01),
                    ));
                }
            });
    }
}
