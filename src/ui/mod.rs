pub mod combat_ui;
pub mod console_log;
pub mod log_ui;

use bevy::prelude::*;

#[derive(Component)]
pub struct HudCombatants;
#[derive(Component)]
pub struct HudPhase;
#[derive(Component)]
pub struct HudLog;
#[derive(Component)]
pub struct HudTurnOrder;

/// Marker on the ability slot container node (index = slot position).
#[derive(Component)]
pub struct AbilitySlot(pub usize);

/// Marker on the Text child inside an AbilitySlot.
#[derive(Component)]
pub struct AbilitySlotLabel(pub usize);

/// Loaded font with Cyrillic support, shared across all HUD text nodes.
#[derive(Resource)]
pub struct UiFont(pub Handle<Font>);
