use super::button::{spawn_standard_button, ButtonStyle};
use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use std::collections::VecDeque;

/// Duration in seconds to slide between two adjacent hexes.
const STEP_DURATION: f32 = 0.12;

// ── Resources ────────────────────────────────────────────────────────────────

/// Queue of pending visual animations. Pipeline is blocked while non-empty.
#[derive(Resource, Default)]
pub struct AnimationQueue(pub VecDeque<PendingAnim>);

pub enum PendingAnim {
    /// Smooth token movement along hex waypoints (pixel coords).
    Movement { token: Entity, waypoints: Vec<Vec2> },
    /// Enemy action popup (text lines).
    Popup { lines: Vec<String> },
}

// ── Components ───────────────────────────────────────────────────────────────

/// Attached to a UnitToken while it is being animated along a path.
#[derive(Component)]
pub struct MovePath {
    pub waypoints: Vec<Vec2>,
    pub index: usize,
    pub t: f32,
}

/// Marker on the enemy action popup root node.
#[derive(Component)]
pub struct EnemyActionPopup;

/// Marker on the dismiss button inside the enemy popup.
#[derive(Component)]
pub struct EnemyPopupDismissButton;

// ── Run condition ────────────────────────────────────────────────────────────

/// Returns true when no animations are active — pipeline may proceed.
pub fn combat_ready(
    queue: Res<AnimationQueue>,
    moving: Query<(), With<MovePath>>,
    popup: Query<(), With<EnemyActionPopup>>,
) -> bool {
    queue.0.is_empty() && moving.is_empty() && popup.is_empty()
}

// ── Systems ──────────────────────────────────────────────────────────────────

/// Pops the next pending animation and starts it.
pub fn process_animation_queue(
    mut commands: Commands,
    mut queue: ResMut<AnimationQueue>,
    moving: Query<(), With<MovePath>>,
    popup: Query<(), With<EnemyActionPopup>>,
    asset_server: Res<AssetServer>,
) {
    // Only start next item when current is done.
    if !moving.is_empty() || !popup.is_empty() {
        return;
    }
    let Some(next) = queue.0.pop_front() else {
        return;
    };
    match next {
        PendingAnim::Movement { token, waypoints } => {
            if waypoints.len() >= 2 {
                commands.entity(token).insert(MovePath {
                    waypoints,
                    index: 0,
                    t: 0.0,
                });
            }
        }
        PendingAnim::Popup { lines } => {
            spawn_enemy_popup(&mut commands, &asset_server, &lines);
        }
    }
}

/// Lerps token Transform along MovePath waypoints.
pub fn animate_movement(
    mut commands: Commands,
    time: Res<Time>,
    mut tokens: Query<(Entity, &mut Transform, &mut MovePath)>,
) {
    for (entity, mut transform, mut path) in &mut tokens {
        path.t += time.delta_secs() / STEP_DURATION;

        while path.t >= 1.0 && path.index < path.waypoints.len() - 1 {
            path.t -= 1.0;
            path.index += 1;
        }

        if path.index >= path.waypoints.len() - 1 {
            // Snap to final position.
            let final_pos = *path.waypoints.last().unwrap();
            transform.translation.x = final_pos.x;
            transform.translation.y = final_pos.y;
            commands.entity(entity).remove::<MovePath>();
            continue;
        }

        let a = path.waypoints[path.index];
        let b = path.waypoints[path.index + 1];
        let t = path.t.clamp(0.0, 1.0);
        let pos = a.lerp(b, t);
        transform.translation.x = pos.x;
        transform.translation.y = pos.y;
    }
}

/// Dismisses enemy popup on Space/Esc or dismiss button click.
pub fn enemy_popup_input(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    popups: Query<Entity, With<EnemyActionPopup>>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<EnemyPopupDismissButton>)>,
) {
    let key_pressed = keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Escape);
    let btn_clicked = buttons.iter().any(|i| *i == Interaction::Pressed);
    if key_pressed || btn_clicked {
        for entity in &popups {
            commands.entity(entity).despawn();
        }
    }
}

// ── Popup UI ─────────────────────────────────────────────────────────────────

fn spawn_enemy_popup(commands: &mut Commands, asset_server: &AssetServer, lines: &[String]) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    let text = lines.join("\n");

    commands
        .spawn((
            EnemyActionPopup,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            ZIndex(200),
            Interaction::default(), // tracked as UI interactive so ui_focus_system sets state on cursor hover
            FocusPolicy::Block,     // explicit (plain Node default is FocusPolicy::Pass)
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::all(Val::Px(24.0)),
                    border: UiRect::all(Val::Px(1.5)),
                    max_width: Val::Px(420.0),
                    ..default()
                },
                BorderColor::all(Color::srgb(0.4, 0.3, 0.2)),
                BackgroundColor(Color::srgb(0.08, 0.07, 0.06)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new(text),
                    TextFont {
                        font: font.clone(),
                        font_size: 15.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.9, 0.85, 0.75)),
                    Node {
                        margin: UiRect::bottom(Val::Px(16.0)),
                        ..default()
                    },
                ));
                spawn_standard_button(
                    panel,
                    font,
                    "Продолжить  [Пробел / Esc]",
                    Val::Auto,
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(EnemyPopupDismissButton);
            });
        });
}
