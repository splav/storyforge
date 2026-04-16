pub mod animation;
pub mod combat_ui;
pub mod console_log;
pub mod hex_grid;
pub mod log_ui;
pub mod story_ui;
pub mod turn_order_ui;

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

/// Marker on each turn-order card slot (index = display position).
#[derive(Component)]
pub struct TurnOrderCard(pub usize);

/// Marker on the name text inside a TurnOrderCard.
#[derive(Component)]
pub struct TurnOrderCardName(pub usize);

/// Marker on the HP text inside a TurnOrderCard.
#[derive(Component)]
pub struct TurnOrderCardHp(pub usize);

/// Marker on the ability slot container node (index = slot position).
#[derive(Component)]
pub struct AbilitySlot(pub usize);

/// Marker on the Text child inside an AbilitySlot.
#[derive(Component)]
pub struct AbilitySlotLabel(pub usize);

/// Root of the defeat overlay (despawned on phase exit).
#[derive(Component)]
pub struct DefeatOverlay;

/// "Сразиться ещё раз" button inside the defeat overlay.
#[derive(Component)]
pub struct RestartButton;

/// Root node of the story screen (despawned on exit).
#[derive(Component)]
pub struct StoryScreenRoot;

/// "Continue" button on the story screen.
#[derive(Component)]
pub struct StoryContinueButton;

/// Marker on the equipment tooltip panel (right side, hidden until a card is hovered).
#[derive(Component)]
pub struct TurnOrderTooltip;

/// Marker on the text node inside the turn-order tooltip.
#[derive(Component)]
pub struct TurnOrderTooltipText;

/// Loaded font with Cyrillic support, shared across all HUD text nodes.
#[derive(Resource)]
pub struct UiFont(pub Handle<Font>);
