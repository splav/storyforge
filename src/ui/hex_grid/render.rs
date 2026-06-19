#![allow(clippy::too_many_arguments)]
use crate::game::components::{
    facing_toward, Combatant, Facing, Faction, StartingHexPos, Team, UnitFigure, UnitSprite,
    UnitToken, VictoryTarget,
};
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
pub const CLR_OBSTACLE: Color = Color::srgba(0.40, 0.25, 0.10, 0.85);
/// Revealed environmental hazard (trap). Dark red/orange, distinct from obstacle brown.
pub const CLR_TRAP: Color = Color::srgba(0.65, 0.10, 0.05, 0.80);

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

/// Marker for a status-badge label slot on a hex cell.
/// `slot` is 0..STATUS_BADGE_SLOTS (left-to-right, bottom-to-top within the row).
#[derive(Component)]
pub struct HexStatusBadge {
    pub slot: u8,
}

/// Number of status-badge slots spawned per hex cell.
pub const STATUS_BADGE_SLOTS: u8 = 4;

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
///
/// `Default` is for test/bench harnesses that don't render — production code
/// fills every handle via `setup_hex_grid`. New fields are zero-cost in tests.
#[derive(Resource, Default)]
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
    pub obstacle: Handle<ColorMaterial>,
    pub trap: Handle<ColorMaterial>,
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
        TextFont {
            font,
            font_size,
            ..default()
        },
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
    cells
        .get(link.0)
        .ok()
        .and_then(|(_, &hex, _)| map.any_at(hex))
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
        obstacle: materials.add(ColorMaterial::from_color(CLR_OBSTACLE)),
        trap: materials.add(ColorMaterial::from_color(CLR_TRAP)),
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

            spawn_hex_label(
                &mut commands,
                cell_id,
                HexNameLabel,
                pixel,
                font.clone(),
                11.0,
                Color::WHITE,
                10.0,
            );
            spawn_hex_label(
                &mut commands,
                cell_id,
                HexHpLabel,
                pixel,
                font.clone(),
                10.0,
                Color::srgb(0.6, 0.9, 0.6),
                -4.0,
            );
            spawn_hex_label(
                &mut commands,
                cell_id,
                HexManaLabel,
                pixel,
                font.clone(),
                9.0,
                Color::srgb(0.85, 0.90, 1.0),
                -16.0,
            );

            let n = STATUS_BADGE_SLOTS as f32;
            let badge_spacing = 14.0_f32;
            let badge_row_start_x = -(n - 1.0) * badge_spacing * 0.5;
            for slot in 0..STATUS_BADGE_SLOTS {
                let badge_x = badge_row_start_x + slot as f32 * badge_spacing;
                commands.spawn((
                    HexCellLink(cell_id),
                    HexStatusBadge { slot },
                    Text2d::new(""),
                    TextFont {
                        font: font.clone(),
                        font_size: 8.0,
                        ..default()
                    },
                    TextLayout::new_with_justify(Justify::Center),
                    TextColor(Color::WHITE),
                    Anchor::CENTER,
                    Transform::from_xyz(pixel.x + badge_x, pixel.y - 27.0, 0.2),
                    Visibility::Hidden,
                ));
            }
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
        ZIndex(100),
    ));

    commands.insert_resource(mats);
    commands.insert_resource(TokenMesh {
        token: token_mesh,
        ring: target_ring_mesh,
    });
}

// ── System: Assign positions ──────────────────────────────────────────────────

/// Resolves a figure's `{facing}`-pending `pattern` into a loadable asset handle.
/// Single home for the `{facing}` substitution + `images/` prefix rule.
fn figure_image(asset_server: &AssetServer, pattern: &str, facing: Facing) -> Handle<Image> {
    let path = pattern.replace("{facing}", facing.token());
    asset_server.load(format!("images/{path}"))
}

/// Spawns the figurine sprite as a child of a unit's token circle, shared by the
/// initial spawn (`assign_hex_positions`) and the bridge summon path. `pattern`
/// still carries the `{facing}` placeholder; facing picks a pre-lit file (no
/// `flip_x` — the scene light is fixed in screen space). `sync_figure_facing`
/// reloads the image when the unit later turns.
/// Fixed on-screen WIDTH of every figure (~hex height × 1.4). Height follows the
/// art's aspect ratio (see `size_unit_figures`), so tall/short creatures keep
/// their proportions while sharing one width. Tunable vs real art.
const FIGURE_WIDTH_PX: f32 = HEX_SIZE * 2.0 * 1.4;
/// Empty padding below the feet as a fraction of the figure's HEIGHT. Source art
/// authored with feet ~this far from the bottom edge; the figure is dropped by
/// it so the feet plant on the token. Trim assets to the feet to push toward 0.
const FIGURE_FEET_PAD: f32 = 0.07;

pub fn spawn_figure_child(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    unit: Entity,
    pattern: &str,
    facing: Facing,
) {
    // Provisional square size/placement until `size_unit_figures` reads the loaded
    // texture aspect (avoids a one-frame native-resolution flash).
    let provisional = -(FIGURE_WIDTH_PX * FIGURE_FEET_PAD + 6.0);
    parent.spawn((
        Sprite {
            image: figure_image(asset_server, pattern, facing),
            custom_size: Some(Vec2::splat(FIGURE_WIDTH_PX)),
            ..default()
        },
        Anchor::BOTTOM_CENTER,
        // above token (abs z 0.17), below world-space badges (abs 0.2)
        Transform::from_xyz(0.0, provisional, 0.02),
        UnitFigure {
            unit,
            pattern: pattern.to_string(),
            facing,
        },
    ));
}

/// Width-normalizes each figure once its texture is loaded: on-screen width is
/// fixed (`FIGURE_WIDTH_PX`), height follows the art's aspect ratio (taller ogres,
/// shorter dwarves stay in proportion). Re-seats the feet (padding scales with
/// height). Idempotent — only writes when the computed size changes.
pub fn size_unit_figures(
    images: Res<Assets<Image>>,
    mut figures: Query<(&mut Sprite, &mut Transform), With<UnitFigure>>,
) {
    for (mut sprite, mut tf) in &mut figures {
        let Some(img) = images.get(&sprite.image) else {
            continue;
        };
        let size = img.size().as_vec2();
        if size.x <= 0.0 {
            continue;
        }
        let height = FIGURE_WIDTH_PX * size.y / size.x;
        let target = Vec2::new(FIGURE_WIDTH_PX, height);
        if sprite.custom_size != Some(target) {
            sprite.custom_size = Some(target);
            tf.translation.y = -(height * FIGURE_FEET_PAD + 6.0);
        }
    }
}

/// Reloads each figure's sprite when its unit's `Facing` changes (turn toward the
/// last interaction). Cheap per-frame comparison; swaps the (cached) asset only on
/// change. On the spawn frame `get` may miss the just-inserted `Facing` command —
/// harmless, the figure already spawned with the correct facing baked in.
pub fn sync_figure_facing(
    facings: Query<&Facing>,
    mut figures: Query<(&mut Sprite, &mut UnitFigure)>,
    asset_server: Option<Res<AssetServer>>,
) {
    let Some(asset_server) = asset_server else {
        return;
    };
    for (mut sprite, mut fig) in &mut figures {
        let Ok(&facing) = facings.get(fig.unit) else {
            continue;
        };
        if facing != fig.facing {
            fig.facing = facing;
            sprite.image = figure_image(&asset_server, &fig.pattern, facing);
        }
    }
}

/// Assigns hex positions and spawns visual tokens for combatants that still
/// have a `StartingHexPos` marker. The marker is removed after assignment,
/// so on subsequent rounds this is a no-op. Also computes each unit's initial
/// `Facing` (toward the nearest opposing-party hex).
#[allow(clippy::type_complexity)]
pub fn assign_hex_positions(
    mut commands: Commands,
    mut positions: ResMut<HexPositions>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    // `Option` so headless test harnesses (MinimalPlugins, no AssetPlugin) can
    // run this system — figures are purely visual and skipped when absent.
    asset_server: Option<Res<AssetServer>>,
    combatants: Query<
        (
            Entity,
            &StartingHexPos,
            &Faction,
            Option<&VictoryTarget>,
            Option<&UnitSprite>,
        ),
        With<Combatant>,
    >,
    grid_offset: Res<HexGridOffset>,
    mats: Res<HexMaterials>,
    token_mesh: Res<TokenMesh>,
) {
    if combatants.is_empty() {
        return;
    }
    positions.clear();
    let asset_server = asset_server.as_deref();

    // Pre-pass: enemy-team hexes, so each unit can face its nearest opponent.
    let enemy_hexes: Vec<Hex> = combatants
        .iter()
        .filter(|(_, _, f, _, _)| f.0 == Team::Enemy)
        .map(|(_, h, _, _, _)| h.0)
        .collect();
    let player_hexes: Vec<Hex> = combatants
        .iter()
        .filter(|(_, _, f, _, _)| f.0 == Team::Player)
        .map(|(_, h, _, _, _)| h.0)
        .collect();

    for (entity, hex_pos, faction, target, sprite) in &combatants {
        positions.insert(entity, hex_pos.0);
        commands.entity(entity).remove::<StartingHexPos>();

        let opponents = if faction.0 == Team::Player {
            &enemy_hexes
        } else {
            &player_hexes
        };
        let facing = opponents
            .iter()
            .min_by_key(|h| hex_pos.0.unsigned_distance_to(**h))
            .map(|nearest| facing_toward(hex_pos.0, *nearest))
            .unwrap_or_else(|| Facing::for_team(faction.0));
        commands.entity(entity).insert(facing);

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
                if let (Some(UnitSprite(pattern)), Some(srv)) = (sprite, asset_server) {
                    spawn_figure_child(parent, srv, entity, pattern, facing);
                }
            });
    }
}
