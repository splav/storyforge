use super::log_ui::LogScrollState;
use super::{
    AbilitySlot, AbilitySlotLabel, DefeatOverlay, HudPhase, HudTurnOrder, LogScrollClip,
    LogScrollThumb, LogText, MoveButton, RestartButton, TurnOrderTooltip, TurnOrderTooltipText,
    UiFont,
};
use super::turn_order_ui::spawn_turn_order_panel;
use crate::app_state::CombatPhase;
use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, StatusOn};
use crate::core::DiceExpr;
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, CombatStats, Combatant, Dead, Equipment, Faction,
    Mana, Rage, Team,
};
use crate::game::messages::RestartCombat;
use crate::game::resources::{GameDb, SelectionState, UiDirty, UiDirtyFlags};
use bevy::prelude::*;

const MAX_SLOTS: usize = 5;

const CLR_SLOT_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_SLOT_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_SLOT_SEL_BG: Color = Color::srgb(0.18, 0.16, 0.06);
const CLR_SLOT_SEL_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
const CLR_SLOT_DIM_BG: Color = Color::srgb(0.08, 0.08, 0.09);
const CLR_SLOT_DIM_BORDER: Color = Color::srgb(0.18, 0.18, 0.20);
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
            // ── Left panel: abilities ─────────────────────────────────────
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(10.0)),
                width: Val::Px(160.0),
                ..default()
            })
            .with_children(|panel| {
                // Move button.
                panel
                    .spawn((
                        MoveButton,
                        Button,
                        Node {
                            border: UiRect::all(Val::Px(1.5)),
                            padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                            width: Val::Percent(100.0),
                            height: Val::Px(36.0),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        BorderColor::all(CLR_SLOT_BORDER),
                        BackgroundColor(CLR_SLOT_BG),
                        Visibility::Hidden,
                    ))
                    .with_children(|btn| {
                        let (tf, tc) = txt(12.0);
                        btn.spawn((Text::new("[M] Движение"), tf, tc));
                    });

                for i in 0..MAX_SLOTS {
                    panel
                        .spawn((
                            AbilitySlot(i),
                            Button,
                            Node {
                                border: UiRect::all(Val::Px(1.5)),
                                padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                                width: Val::Percent(100.0),
                                height: Val::Px(70.0),
                                flex_direction: FlexDirection::Column,
                                justify_content: JustifyContent::SpaceBetween,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BorderColor::all(CLR_SLOT_BORDER),
                            BackgroundColor(CLR_SLOT_BG),
                            Visibility::Hidden,
                        ))
                        .with_children(|slot| {
                            let (tf, tc) = txt(12.0);
                            slot.spawn((AbilitySlotLabel(i), Text::new(""), tf, tc));
                        });
                }
            });

            // ── Center: spacer (hex grid shows through) ───────────────────
            root.spawn(Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            })
            .with_children(|center| {
                // Phase / hint
                let (tf, _) = txt(14.0);
                center.spawn((HudPhase, Text::new(""), tf, TextColor(CLR_HINT)));

                // Hidden legacy marker (kept so update_turn_order can find it)
                center.spawn((HudTurnOrder, Node { display: Display::None, ..default() }));

                center.spawn(Node {
                    flex_grow: 1.0,
                    ..default()
                });

                // Combat log — row: clip area + scrollbar track
                {
                    let (tf, _) = txt(13.0);
                    center
                        .spawn((
                            Node {
                                height: Val::Px(110.0),
                                flex_direction: FlexDirection::Row,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.3)),
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

            if actor_info.is_none() {
                format!("Ход: {actor_name}")
            } else {
                let ap = actor_info.unwrap().2;
                let mut hints = Vec::new();
                if ap.movement {
                    hints.push("[M]: движение");
                }
                if ap.action {
                    hints.push("[1-5]: способность");
                }
                hints.push("[E]: конец хода");
                if sel.move_mode {
                    hints.push("Клик: выбрать клетку");
                } else if sel.selected_ability.is_some() && sel.selected_target.is_some() {
                    hints.push("Enter: подтвердить");
                }
                format!("Ход: {actor_name}  |  {}", hints.join("  "))
            }
        }
        CombatPhase::Victory => "★  ПОБЕДА  (Space)".into(),
        CombatPhase::Defeat => "✗  ПОРАЖЕНИЕ  (Space)".into(),
        p => format!("{p:?}"),
    };
}

// ── Update: combatants list ───────────────────────────────────────────────────

// ── Update: ability panel ─────────────────────────────────────────────────────

pub fn update_ability_panel(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    db: Res<GameDb>,
    combatants: Query<
        (
            &Faction,
            &Abilities,
            &CombatStats,
            Option<&Equipment>,
            Option<&Mana>,
            Option<&Rage>,
        ),
        (With<Combatant>, Without<Dead>),
    >,
    mut slots: Query<(
        &AbilitySlot,
        &mut Node,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Visibility,
    )>,
    mut labels: Query<(&AbilitySlotLabel, &mut Text, &mut TextColor)>,
) {
    if !dirty.0.contains(UiDirtyFlags::ABILITY_PANEL) {
        return;
    }
    let actor_data = active_q
        .single()
        .ok()
        .and_then(|e| combatants.get(e).ok())
        .filter(|(f, _, _, _, _, _)| f.0 == Team::Player)
        .map(|(_, abilities, stats, equip, mana, rage)| {
            let ctx = CasterContext::new(stats, equip, &db.weapons);
            (
                abilities.0.clone(),
                ctx,
                mana.map(|m| m.current),
                rage.map(|r| r.current),
            )
        });

    let (abilities, caster_ctx, mana_cur, rage_cur) = match actor_data {
        Some(d) => d,
        None => return,
    };

    // Helper: can the actor afford this ability?
    let can_use = |def: &AbilityDef| -> bool {
        if def.mana_cost > 0 && mana_cur.unwrap_or(0) < def.mana_cost {
            return false;
        }
        if def.rage_cost > 0 && rage_cur.unwrap_or(0) < def.rage_cost {
            return false;
        }
        true
    };

    for (slot, mut node, mut border, mut bg, mut vis) in &mut slots {
        let idx = slot.0;
        let ability_id = abilities.get(idx).cloned();
        let selected = ability_id.is_some() && sel.selected_ability == ability_id;
        let affordable = ability_id
            .as_ref()
            .and_then(|id| db.abilities.get(id))
            .map(|def| can_use(def))
            .unwrap_or(false);

        *vis = if ability_id.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        node.border = if selected {
            UiRect::all(Val::Px(2.5))
        } else {
            UiRect::all(Val::Px(1.5))
        };
        *border = BorderColor::all(if selected {
            CLR_SLOT_SEL_BORDER
        } else if affordable {
            CLR_SLOT_BORDER
        } else {
            CLR_SLOT_DIM_BORDER
        });
        *bg = BackgroundColor(if selected {
            CLR_SLOT_SEL_BG
        } else if affordable {
            CLR_SLOT_BG
        } else {
            CLR_SLOT_DIM_BG
        });
    }

    for (label, mut text, mut color) in &mut labels {
        let idx = label.0;
        if let Some(id) = abilities.get(idx).cloned() {
            if let Some(def) = db.abilities.get(&id) {
                let effect_str = ability_effect_str(def, &caster_ctx, &db);
                let mut costs = String::new();
                if def.rage_cost > 0 {
                    costs += &format!(" R:{}", def.rage_cost);
                }
                if def.mana_cost > 0 {
                    costs += &format!(" M:{}", def.mana_cost);
                }
                text.0 = format!("[{}] {}{}\n{}", idx + 1, def.name, costs, effect_str);
                let affordable = can_use(def);
                let selected = sel.selected_ability == Some(id);
                *color = TextColor(if selected {
                    Color::srgb(1.0, 0.95, 0.5)
                } else if affordable {
                    Color::WHITE
                } else {
                    Color::srgb(0.35, 0.35, 0.38)
                });
            }
        }
    }
}

fn ability_effect_str(
    def: &AbilityDef,
    ctx: &CasterContext,
    db: &GameDb,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(calc) = def.effect.calc(ctx) {
        let label = if calc.is_heal {
            "лечение"
        } else if calc.pierces_armor {
            "урон (закл.)"
        } else {
            "урон"
        };
        let s = if let Some(ref dice) = calc.dice {
            format!("{} {label}", dice_bonus_str(dice, calc.bonus))
        } else {
            format!("{} {label}", calc.bonus)
        };
        lines.push(s);
    } else if let EffectDef::GrantMovement { distance } = &def.effect {
        lines.push(format!("движение +{distance}"));
    }

    for sa in &def.statuses {
        let status_name = db
            .statuses
            .get(&sa.status)
            .map(|s| s.name.as_str())
            .unwrap_or("?");
        let on_str = match sa.on {
            StatusOn::MySelf => "себя",
            StatusOn::Target => "цель",
        };
        lines.push(format!(
            "→ {} на {} ({} ход.)",
            status_name, on_str, sa.duration_rounds
        ));
    }

    if lines.is_empty() {
        "—".into()
    } else {
        lines.join("\n    ")
    }
}

fn dice_bonus_str(dice: &DiceExpr, bonus: i32) -> String {
    match bonus.cmp(&0) {
        std::cmp::Ordering::Greater => format!("{}d{}+{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Less => format!("{}d{}{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Equal => format!("{}d{}", dice.count, dice.sides),
    }
}

// ── Ability slot click ────────────────────────────────────────────────────────

pub fn ability_slot_click_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut sel: ResMut<SelectionState>,
    slots: Query<(&AbilitySlot, &Interaction), Changed<Interaction>>,
    combatants: Query<(&Faction, &Abilities), (With<Combatant>, Without<Dead>)>,
) {
    for (slot, interaction) in &slots {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Ok(active) = active_q.single() else { continue };
        let Ok((faction, abilities)) = combatants.get(active) else {
            continue;
        };
        if faction.0 != Team::Player {
            continue;
        }
        if let Some(id) = abilities.0.get(slot.0).cloned() {
            sel.selected_ability = Some(id);
            sel.move_mode = false;
        }
    }
}

// ── Move button ─────────────────────────────────────────────────────────────

pub fn update_move_button(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    combatants: Query<(&Faction, &ActionPoints), (With<Combatant>, Without<Dead>)>,
    mut move_btn: Query<
        (&mut BorderColor, &mut BackgroundColor, &mut Visibility),
        With<MoveButton>,
    >,
) {
    if !dirty.0.contains(UiDirtyFlags::MOVE_BTN) {
        return;
    }
    let Ok((mut border, mut bg, mut vis)) = move_btn.single_mut() else {
        return;
    };

    let is_player_turn = active_q
        .single()
        .ok()
        .and_then(|e| combatants.get(e).ok())
        .is_some_and(|(f, _)| f.0 == Team::Player);

    if !is_player_turn {
        *vis = Visibility::Hidden;
        return;
    }
    *vis = Visibility::Visible;

    let has_movement = active_q
        .single()
        .ok()
        .and_then(|e| combatants.get(e).ok())
        .map_or(false, |(_, ap)| ap.movement);

    if sel.move_mode {
        *border = BorderColor::all(CLR_SLOT_SEL_BORDER);
        *bg = BackgroundColor(CLR_SLOT_SEL_BG);
    } else if has_movement {
        *border = BorderColor::all(CLR_SLOT_BORDER);
        *bg = BackgroundColor(CLR_SLOT_BG);
    } else {
        *border = BorderColor::all(CLR_SLOT_DIM_BORDER);
        *bg = BackgroundColor(CLR_SLOT_DIM_BG);
    }
}

pub fn move_button_click_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut sel: ResMut<SelectionState>,
    move_btn: Query<&Interaction, (Changed<Interaction>, With<MoveButton>)>,
    combatants: Query<(&Faction, &ActionPoints), (With<Combatant>, Without<Dead>)>,
) {
    for interaction in &move_btn {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Ok(active) = active_q.single() else { continue };
        let Ok((faction, ap)) = combatants.get(active) else {
            continue;
        };
        if faction.0 != Team::Player || !ap.movement {
            continue;
        }

        if sel.move_mode {
            sel.move_mode = false;
        } else {
            sel.move_mode = true;
            sel.selected_ability = None;
            sel.selected_target = None;
        }
    }
}

// ── Defeat overlay ────────────────────────────────────────────────────────────

const CLR_OVERLAY_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.72);
const CLR_BTN_BG: Color = Color::srgb(0.14, 0.08, 0.08);
const CLR_BTN_BORDER: Color = Color::srgb(0.60, 0.18, 0.18);
const CLR_BTN_HOV_BG: Color = Color::srgb(0.22, 0.10, 0.10);
const CLR_BTN_HOV_BORDER: Color = Color::srgb(0.85, 0.25, 0.25);
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
                panel
                    .spawn((
                        RestartButton,
                        Button,
                        Node {
                            padding: UiRect::axes(Val::Px(28.0), Val::Px(12.0)),
                            border: UiRect::all(Val::Px(1.5)),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        BorderColor::all(CLR_BTN_BORDER),
                        BackgroundColor(CLR_BTN_BG),
                    ))
                    .with_children(|btn| {
                        btn.spawn((
                            Text::new("Сразиться ещё раз"),
                            TextFont { font: font.clone(), font_size: 16.0, ..default() },
                            TextColor(Color::WHITE),
                        ));
                    });

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

pub fn defeat_button_hover(
    mut buttons: Query<
        (&Interaction, &mut BorderColor, &mut BackgroundColor),
        (Changed<Interaction>, With<RestartButton>),
    >,
) {
    for (interaction, mut border, mut bg) in &mut buttons {
        match interaction {
            Interaction::Hovered => {
                *border = BorderColor::all(CLR_BTN_HOV_BORDER);
                *bg = BackgroundColor(CLR_BTN_HOV_BG);
            }
            _ => {
                *border = BorderColor::all(CLR_BTN_BORDER);
                *bg = BackgroundColor(CLR_BTN_BG);
            }
        }
    }
}
