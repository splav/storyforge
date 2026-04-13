pub mod combat_ui;
pub mod console_log;
pub mod hex_grid;
pub mod log_ui;
pub mod story_ui;

use bevy::prelude::*;

#[derive(Component)]
pub struct HudPhase;
/// Marker on the clip container that handles overflow clipping.
#[derive(Component)]
pub struct LogScrollClip;

/// Marker on the Text node inside the scroll clip.
#[derive(Component)]
pub struct LogText;

/// Marker on the scrollbar thumb.
#[derive(Component)]
pub struct LogScrollThumb;
#[derive(Component)]
pub struct HudTurnOrder;

/// Marker on the ability slot container node (index = slot position).
#[derive(Component)]
pub struct AbilitySlot(pub usize);

/// Marker on the Text child inside an AbilitySlot.
#[derive(Component)]
pub struct AbilitySlotLabel(pub usize);

/// Marker on the "Move" button in the ability panel.
#[derive(Component)]
pub struct MoveButton;

/// Root node of the story screen (despawned on exit).
#[derive(Component)]
pub struct StoryScreenRoot;

/// "Continue" button on the story screen.
#[derive(Component)]
pub struct StoryContinueButton;

/// Loaded font with Cyrillic support, shared across all HUD text nodes.
#[derive(Resource)]
pub struct UiFont(pub Handle<Font>);
