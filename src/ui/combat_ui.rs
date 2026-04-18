#![allow(clippy::type_complexity)]
use super::ability_panel::spawn_ability_panel;
use super::log_ui::LogScrollState;
use super::{
    DefeatOverlay, HudPhase, HudTurnOrder, LogScrollClip, LogScrollThumb, LogText, RestartButton,
    TurnOrderTooltip, TurnOrderTooltipText, UiFont,
};
use super::turn_order_ui::spawn_turn_order_panel;
use crate::app_state::CombatPhase;
use crate::game::components::{ActionPoints, ActiveCombatant, Combatant, Faction, Team};
use crate::game::messages::RestartCombat;
use crate::game::resources::{CombatObjective, SelectionState, UiDirty, UiDirtyFlags};
use bevy::prelude::*;

const CLR_HINT: Color = Color::srgb(0.55, 0.55, 0.30);

pub fn setup_hud(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    commands.insert_resource(UiFont(font.clone()));

    let txt = |size: f32| -> (TextFont, TextColor) {
        (
            TextFont {
                font: font.clone(),
                font_size: size,
                ..default()
            },
            TextColor(Color::WHITE),
        )
    };

    // ── Root: full screen, row layout ────────────────────────────────────────
    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            ..default()
        })
        .with_children(|root| {
            // ── Left panel: abilities + description ───────────────────────
            spawn_ability_panel(root, &font);

            // ── Center: spacer (hex grid shows through) ───────────────────
            root.spawn(Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            })
            .with_children(|center| {
                // Combat log — row: clip area + scrollbar track (top)
                {
                    let (tf, _) = txt(13.0);
                    center
                        .spawn((
                            Node {
                                height: Val::Px(110.0),
                                flex_direction: FlexDirection::Row,
                                border: UiRect::all(Val::Px(1.0)),
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BorderColor::all(Color::srgb(0.22, 0.22, 0.26)),
                            BackgroundColor(Color::srgba(0.07, 0.07, 0.09, 0.92)),
                        ))
                        .with_children(|container| {
                            // Scroll clip area
                            container
                                .spawn((
                                    LogScrollClip,
                                    LogScrollState::default(),
                                    Button,
                                    Node {
                                        flex_grow: 1.0,
                                        height: Val::Percent(100.0),
                                        overflow: Overflow::scroll_y(),
                                        flex_direction: FlexDirection::Column,
                                        ..default()
                                    },
                                ))
                                .with_children(|clip| {
                                    clip.spawn((
                                        LogText,
                                        Text::new(""),
                                        tf,
                                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                                    ));
                                });

                            // Scrollbar track
                            container
                                .spawn(Node {
                                    width: Val::Px(6.0),
                                    height: Val::Percent(100.0),
                                    ..default()
                                })
                                .with_children(|track| {
                                    track.spawn((
                                        LogScrollThumb,
                                        Node {
                                            position_type: PositionType::Absolute,
                                            width: Val::Percent(100.0),
                                            top: Val::Px(0.0),
                                            height: Val::Percent(100.0),
                                            border_radius: BorderRadius::all(Val::Px(2.0)),
                                            ..default()
                                        },
                                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.35)),
                                    ));
                                });
                        });
                }

                // Hidden legacy marker (kept so update_turn_order can find it)
                center.spawn((HudTurnOrder, Node { display: Display::None, ..default() }));

                center.spawn(Node {
                    flex_grow: 1.0,
                    ..default()
                });

                // Phase / hint (bottom)
                let (tf, _) = txt(14.0);
                center.spawn((HudPhase, Text::new(""), tf, TextColor(CLR_HINT)));
            });

            // ── Right panel: turn order cards ─────────────────────────────
            spawn_turn_order_panel(root, &font);
        });

    // ── Equipment tooltip (absolute, hidden until card is hovered) ───────────
    commands.spawn((
        TurnOrderTooltip,
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(172.0),
            top: Val::Px(8.0),
            padding: UiRect::all(Val::Px(8.0)),
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BorderColor::all(Color::srgb(0.32, 0.32, 0.38)),
        BackgroundColor(Color::srgba(0.07, 0.07, 0.09, 0.96)),
        Visibility::Hidden,
        ZIndex(50),
    ))
    .with_children(|tooltip| {
        tooltip.spawn((
            TurnOrderTooltipText,
            Text::new(""),
            TextFont {
                font: font.clone(),
                font_size: 11.0,
                ..default()
            },
            TextColor(Color::srgb(0.82, 0.82, 0.88)),
        ));
    });
}

// ── Update: phase hint ────────────────────────────────────────────────────────

pub fn update_phase_hint(
    dirty: Res<UiDirty>,
    phase: Res<State<CombatPhase>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    objective: Res<CombatObjective>,
    combatants: Query<(&Name, &Faction, &ActionPoints), With<Combatant>>,
    mut phase_q: Query<&mut Text, With<HudPhase>>,
) {
    if !dirty.0.contains(UiDirtyFlags::PHASE_HINT) {
        return;
    }
    let Ok(mut t) = phase_q.single_mut() else {
        return;
    };
    t.0 = match phase.get() {
        CombatPhase::AwaitCommand => {
            let actor_info = active_q
                .single()
                .ok()
                .and_then(|e| combatants.get(e).ok())
                .filter(|(_, f, _)| f.0 == Team::Player);
            let actor_name = actor_info
                .map(|(n, _, _)| n.as_str())
                .unwrap_or("Враг");

            let goal = objective.0.objective_text();
            if actor_info.is_some() {
                let mut hints: Vec<&str> = Vec::new();
                if sel.move_mode {
                    hints.push("Клик: выбрать клетку");
                } else if sel.selected_ability.is_some() && sel.selected_target.is_some() {
                    hints.push("Enter: подтвердить");
                }
                let head = if hints.is_empty() {
                    format!("Ход: {actor_name}")
                } else {
                    format!("Ход: {actor_name}  |  {}", hints.join("  "))
                };
                format!("{head}\nЦель: {goal}")
            } else {
                format!("Ход: {actor_name}\nЦель: {goal}")
            }
        }
        CombatPhase::Victory => "★  ПОБЕДА  (Space)".into(),
        CombatPhase::Defeat => "✗  ПОРАЖЕНИЕ  (Space)".into(),
        p => format!("{p:?}"),
    };
}

// ── Defeat overlay ────────────────────────────────────────────────────────────

const CLR_OVERLAY_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.72);
const CLR_MENU_BG: Color = Color::srgb(0.08, 0.06, 0.06);
const CLR_MENU_BORDER: Color = Color::srgb(0.35, 0.20, 0.20);

pub fn setup_defeat_overlay(mut commands: Commands, font: Res<UiFont>) {
    let font = font.0.clone();

    commands
        .spawn((
            DefeatOverlay,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(CLR_OVERLAY_BG),
            ZIndex(100),
        ))
        .with_children(|root| {
            // Central panel
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(Val::Px(48.0), Val::Px(36.0)),
                    row_gap: Val::Px(20.0),
                    border: UiRect::all(Val::Px(1.5)),
                    ..default()
                },
                BorderColor::all(CLR_MENU_BORDER),
                BackgroundColor(CLR_MENU_BG),
            ))
            .with_children(|panel| {
                // Title
                panel.spawn((
                    Text::new("✗  ПОРАЖЕНИЕ"),
                    TextFont { font: font.clone(), font_size: 28.0, ..default() },
                    TextColor(Color::srgb(0.85, 0.25, 0.20)),
                ));

                // "Сразиться ещё раз" button
                super::button::spawn_standard_button(
                    panel,
                    font.clone(),
                    "Сразиться ещё раз",
                    Val::Auto,
                    Val::Auto,
                    super::button::ButtonStyle::Danger,
                )
                .insert(RestartButton);

                // Hint
                panel.spawn((
                    Text::new("[R] — сразиться ещё раз   [Esc] — главное меню"),
                    TextFont { font, font_size: 12.0, ..default() },
                    TextColor(Color::srgb(0.45, 0.45, 0.45)),
                ));
            });
        });
}

pub fn cleanup_defeat_overlay(
    mut commands: Commands,
    overlays: Query<Entity, With<DefeatOverlay>>,
) {
    for entity in &overlays {
        commands.entity(entity).despawn();
    }
}

pub fn defeat_overlay_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<RestartButton>)>,
    mut restart_writer: MessageWriter<RestartCombat>,
    mut next_state: ResMut<NextState<crate::app_state::AppState>>,
) {
    let restart = keys.just_pressed(KeyCode::KeyR)
        || buttons.iter().any(|i| *i == Interaction::Pressed);
    let to_menu = keys.just_pressed(KeyCode::Escape);

    if restart {
        restart_writer.write(RestartCombat);
    } else if to_menu {
        next_state.set(crate::app_state::AppState::MainMenu);
    }
}

