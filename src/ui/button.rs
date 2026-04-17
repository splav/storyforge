use bevy::ecs::system::EntityCommands;
use bevy::prelude::*;

const DEFAULT_BORDER: Color = Color::srgb(0.40, 0.40, 0.30);
const DEFAULT_BG: Color = Color::srgb(0.12, 0.12, 0.10);
const DANGER_BORDER: Color = Color::srgb(0.60, 0.18, 0.18);
const DANGER_BG: Color = Color::srgb(0.14, 0.08, 0.08);
const TEXT_COLOR: Color = Color::WHITE;
const FONT_SIZE: f32 = 16.0;

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Default,
    Danger,
}

impl ButtonStyle {
    fn colors(self) -> (Color, Color) {
        match self {
            Self::Default => (DEFAULT_BORDER, DEFAULT_BG),
            Self::Danger => (DANGER_BORDER, DANGER_BG),
        }
    }
}

/// Spawns a standard button with configurable size and style.
/// Use `Val::Auto` for width/height to size to content (with padding fallback).
pub fn spawn_standard_button<'a>(
    parent: &'a mut ChildSpawnerCommands,
    font: Handle<Font>,
    text: impl Into<String>,
    width: Val,
    height: Val,
    style: ButtonStyle,
) -> EntityCommands<'a> {
    let (border, bg) = style.colors();
    let mut ec = parent.spawn((
        Button,
        Node {
            width,
            height,
            padding: UiRect::axes(Val::Px(24.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.5)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BorderColor::all(border),
        BackgroundColor(bg),
    ));
    ec.with_children(|btn| {
        btn.spawn((
            Text::new(text),
            TextFont {
                font,
                font_size: FONT_SIZE,
                ..default()
            },
            TextColor(TEXT_COLOR),
        ));
    });
    ec
}
