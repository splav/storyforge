use super::{TurnOrderCard, TurnOrderCardHp, TurnOrderCardName};
use crate::game::components::{Combatant, Dead, Faction, Team, Vital};
use crate::game::resources::{TurnQueue, UiDirty, UiDirtyFlags};
use bevy::prelude::*;

pub const MAX_TURN_CARDS: usize = 8;

const CLR_CARD_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_CARD_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_CARD_PLAYER_BG: Color = Color::srgb(0.08, 0.10, 0.16);
const CLR_CARD_PLAYER_BORDER: Color = Color::srgb(0.25, 0.35, 0.60);
const CLR_CARD_ENEMY_BG: Color = Color::srgb(0.16, 0.08, 0.08);
const CLR_CARD_ENEMY_BORDER: Color = Color::srgb(0.55, 0.20, 0.20);
const CLR_CARD_ACTIVE_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
const CLR_CARD_DEAD_BG: Color = Color::srgb(0.06, 0.06, 0.07);
const CLR_CARD_DEAD_BORDER: Color = Color::srgb(0.18, 0.18, 0.20);

/// Spawns the right-side turn order panel as a child of the root row node.
pub fn spawn_turn_order_panel(parent: &mut ChildSpawnerCommands, font: &Handle<Font>) {
    let txt = |size: f32| -> TextFont {
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        }
    };

    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            justify_content: JustifyContent::FlexStart,
            row_gap: Val::Px(4.0),
            padding: UiRect::all(Val::Px(8.0)),
            width: Val::Px(160.0),
            ..default()
        })
        .with_children(|panel| {
            for i in 0..MAX_TURN_CARDS {
                panel
                    .spawn((
                        TurnOrderCard(i),
                        Node {
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::Center,
                            padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                            border: UiRect::all(Val::Px(1.5)),
                            width: Val::Percent(100.0),
                            min_height: Val::Px(44.0),
                            ..default()
                        },
                        BorderColor::all(CLR_CARD_BORDER),
                        BackgroundColor(CLR_CARD_BG),
                        Visibility::Hidden,
                    ))
                    .with_children(|card| {
                        card.spawn((
                            TurnOrderCardName(i),
                            Text::new(""),
                            txt(12.0),
                            TextColor(Color::WHITE),
                        ));
                        card.spawn((
                            TurnOrderCardHp(i),
                            Text::new(""),
                            txt(11.0),
                            TextColor(Color::srgb(0.55, 0.85, 0.55)),
                        ));
                    });
            }
        });
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn update_turn_order(
    dirty: Res<UiDirty>,
    queue: Res<TurnQueue>,
    combatants: Query<(&Name, &Vital, &Faction, Has<Dead>), With<Combatant>>,
    mut cards: Query<(
        &TurnOrderCard,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Visibility,
    )>,
    mut name_texts: Query<(&TurnOrderCardName, &mut Text, &mut TextColor)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TURN_ORDER) {
        return;
    }

    let len = queue.order.len();
    let display: Vec<(usize, bool)> = (0..len.min(MAX_TURN_CARDS))
        .map(|i| ((queue.index + i) % len, i == 0))
        .collect();

    for (card, mut border, mut bg, mut vis) in &mut cards {
        let slot = card.0;
        if let Some(&(queue_idx, is_active)) = display.get(slot) {
            let entity = queue.order[queue_idx];
            if let Ok((_, _, faction, is_dead)) = combatants.get(entity) {
                *vis = Visibility::Visible;
                if is_dead {
                    *border = BorderColor::all(CLR_CARD_DEAD_BORDER);
                    *bg = BackgroundColor(CLR_CARD_DEAD_BG);
                } else if is_active {
                    *border = BorderColor::all(CLR_CARD_ACTIVE_BORDER);
                    *bg = BackgroundColor(match faction.0 {
                        Team::Player => CLR_CARD_PLAYER_BG,
                        Team::Enemy => CLR_CARD_ENEMY_BG,
                    });
                } else {
                    let (bg_clr, border_clr) = match faction.0 {
                        Team::Player => (CLR_CARD_PLAYER_BG, CLR_CARD_PLAYER_BORDER),
                        Team::Enemy => (CLR_CARD_ENEMY_BG, CLR_CARD_ENEMY_BORDER),
                    };
                    *border = BorderColor::all(border_clr);
                    *bg = BackgroundColor(bg_clr);
                }
            }
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (label, mut text, mut color) in &mut name_texts {
        let slot = label.0;
        if let Some(&(queue_idx, is_active)) = display.get(slot) {
            let entity = queue.order[queue_idx];
            if let Ok((name, _, faction, is_dead)) = combatants.get(entity) {
                let prefix = if is_active { "▶ " } else { "  " };
                let team_icon = match faction.0 {
                    Team::Player => "⚔",
                    Team::Enemy => "☠",
                };
                let dead_suffix = if is_dead { " ✗" } else { "" };
                text.0 = format!("{prefix}{team_icon} {}{dead_suffix}", name.as_str());
                *color = TextColor(if is_dead {
                    Color::srgb(0.35, 0.35, 0.38)
                } else if is_active {
                    Color::srgb(1.0, 0.95, 0.5)
                } else {
                    Color::WHITE
                });
            }
        } else {
            text.0.clear();
        }
    }
}

pub fn update_turn_order_hp(
    dirty: Res<UiDirty>,
    queue: Res<TurnQueue>,
    combatants: Query<(&Vital, Has<Dead>), With<Combatant>>,
    mut hp_q: Query<(&TurnOrderCardHp, &mut Text, &mut TextColor)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TURN_ORDER) {
        return;
    }
    let len = queue.order.len();
    for (label, mut text, mut color) in &mut hp_q {
        let slot = label.0;
        if slot < len.min(MAX_TURN_CARDS) {
            let queue_idx = (queue.index + slot) % len;
            let entity = queue.order[queue_idx];
            if let Ok((vital, is_dead)) = combatants.get(entity) {
                text.0 = format!("HP {}/{}", vital.hp, vital.max_hp);
                *color = TextColor(if is_dead {
                    Color::srgb(0.28, 0.28, 0.30)
                } else {
                    let ratio = vital.hp as f32 / vital.max_hp.max(1) as f32;
                    if ratio > 0.5 {
                        Color::srgb(0.40, 0.80, 0.40)
                    } else if ratio > 0.25 {
                        Color::srgb(0.85, 0.70, 0.20)
                    } else {
                        Color::srgb(0.85, 0.25, 0.20)
                    }
                });
            }
        } else {
            text.0.clear();
        }
    }
}
