#![allow(clippy::type_complexity)]
use super::ability_panel::spawn_ability_panel;
use super::inspect_panel::spawn_inspect_panel;
use super::log_ui::LogScrollState;
use super::turn_order_ui::spawn_turn_order_panel;
use super::{
    DefeatOverlay, HudPhase, HudTurnOrder, LogScrollClip, LogScrollThumb, LogText, ProceedButton,
    RestartButton, TurnOrderTooltip, TurnOrderTooltipText, UiFont,
};
use crate::app_state::CombatPhase;
use crate::game::components::{ActionPoints, ActiveCombatant, Combatant, Faction, Team};
use crate::game::messages::RestartCombat;
use crate::game::resources::{
    CombatContext, CombatObjective, GameDb, PhaseDeadline, ScenarioState, SelectionState, UiDirty,
    UiDirtyFlags,
};
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
                center.spawn((
                    HudTurnOrder,
                    Node {
                        display: Display::None,
                        ..default()
                    },
                ));

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

    // ── Inspection panel (absolute, bottom-right, hidden until a unit is clicked) ──
    spawn_inspect_panel(&mut commands, &font);

    // ── Equipment tooltip (absolute, hidden until card is hovered) ───────────
    commands
        .spawn((
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

#[allow(clippy::too_many_arguments)]
pub fn update_phase_hint(
    dirty: Res<UiDirty>,
    phase: Res<State<CombatPhase>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    objective: Res<CombatObjective>,
    deadline: Res<PhaseDeadline>,
    combat_ctx: Res<CombatContext>,
    combatants: Query<(&Name, &Faction, &ActionPoints), With<Combatant>>,
    mut phase_q: Query<&mut Text, With<HudPhase>>,
) {
    if !dirty.0.contains(UiDirtyFlags::PHASE_HINT) {
        return;
    }

    // Round-based phase deadline (e.g. "kill the fleeing boss in N rounds"):
    // append the remaining-rounds counter so the player can see the timer.
    // `PHASE_HINT` is re-dirtied on every active-combatant change (see
    // `hex_grid::visuals`), so this ticks down each round automatically.
    let deadline_suffix = deadline.0.as_ref().map(|dl| {
        let elapsed = combat_ctx.round.saturating_sub(dl.phase_started_round);
        let left = dl.limit.saturating_sub(elapsed);
        format!("  (осталось раундов: {left})")
    });
    let Ok(mut t) = phase_q.single_mut() else {
        return;
    };
    // Objective text is phase-independent; compute it once so transient phases
    // (e.g. StartRound) can show the goal too rather than leaking a raw phase
    // Debug name into the HUD.
    let mut goal = objective.0.objective_text();
    if let Some(suffix) = &deadline_suffix {
        goal.push_str(suffix);
    }

    t.0 = match phase.get() {
        CombatPhase::AwaitCommand => {
            let actor_info = active_q
                .single()
                .ok()
                .and_then(|e| combatants.get(e).ok())
                .filter(|(_, f, _)| f.0 == Team::Player);
            let actor_name = actor_info.map(|(n, _, _)| n.as_str()).unwrap_or("Враг");

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
        // Transient phases (e.g. StartRound): show the objective, not the raw
        // Debug phase name (which previously leaked "StartRound" into the HUD).
        _ => format!("Цель: {goal}"),
    };
}

// ── Defeat overlay ────────────────────────────────────────────────────────────

const CLR_OVERLAY_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.72);
const CLR_MENU_BG: Color = Color::srgb(0.08, 0.06, 0.06);
const CLR_MENU_BORDER: Color = Color::srgb(0.35, 0.20, 0.20);

pub fn setup_defeat_overlay(
    mut commands: Commands,
    font: Res<UiFont>,
    db: Res<GameDb>,
    scenario: Option<Res<ScenarioState>>,
) {
    let font = font.0.clone();

    let on_defeat = scenario
        .as_ref()
        .map(|s| crate::scenario::current_on_defeat(&db, s))
        .unwrap_or(crate::content::encounters::OnDefeat::Retry);

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
                    TextFont {
                        font: font.clone(),
                        font_size: 28.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.25, 0.20)),
                ));

                match on_defeat {
                    crate::content::encounters::OnDefeat::Retry => {
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
                            TextFont {
                                font,
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.45, 0.45, 0.45)),
                        ));
                    }
                    crate::content::encounters::OnDefeat::Proceed => {
                        // "Продолжить" button
                        super::button::spawn_standard_button(
                            panel,
                            font.clone(),
                            "Продолжить",
                            Val::Auto,
                            Val::Auto,
                            super::button::ButtonStyle::Default,
                        )
                        .insert(ProceedButton);

                        // Hint
                        panel.spawn((
                            Text::new("[Space] — продолжить   [Esc] — главное меню"),
                            TextFont {
                                font,
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.45, 0.45, 0.45)),
                        ));
                    }
                }
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

#[allow(clippy::too_many_arguments)]
pub fn defeat_overlay_input(
    keys: Res<ButtonInput<KeyCode>>,
    restart_buttons: Query<&Interaction, (Changed<Interaction>, With<RestartButton>)>,
    proceed_buttons: Query<&Interaction, (Changed<Interaction>, With<ProceedButton>)>,
    mut restart_writer: MessageWriter<RestartCombat>,
    mut advance_writer: MessageWriter<crate::scenario::AdvanceScenario>,
    mut next_state: ResMut<NextState<crate::app_state::AppState>>,
    db: Res<GameDb>,
    scenario: Option<Res<ScenarioState>>,
) {
    let on_defeat = scenario
        .as_ref()
        .map(|s| crate::scenario::current_on_defeat(&db, s))
        .unwrap_or(crate::content::encounters::OnDefeat::Retry);
    let to_menu = keys.just_pressed(KeyCode::Escape);
    match on_defeat {
        crate::content::encounters::OnDefeat::Proceed => {
            let go = keys.just_pressed(KeyCode::Space)
                || keys.just_pressed(KeyCode::Enter)
                || proceed_buttons.iter().any(|i| *i == Interaction::Pressed);
            if go {
                advance_writer.write(crate::scenario::AdvanceScenario);
            } else if to_menu {
                next_state.set(crate::app_state::AppState::MainMenu);
            }
        }
        crate::content::encounters::OnDefeat::Retry => {
            let retry = keys.just_pressed(KeyCode::KeyR)
                || restart_buttons.iter().any(|i| *i == Interaction::Pressed);
            if retry {
                restart_writer.write(RestartCombat);
            } else if to_menu {
                next_state.set(crate::app_state::AppState::MainMenu);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::encounters::{EncounterDef, OnDefeat, VictoryCondition};
    use crate::game::resources::{GameDb, ScenarioState};
    use crate::scenario::AdvanceScenario;
    use std::collections::HashMap;

    #[derive(Resource, Default)]
    struct AdvanceCount(usize);
    #[derive(Resource, Default)]
    struct RestartCount(usize);

    fn count_advance(mut r: MessageReader<AdvanceScenario>, mut c: ResMut<AdvanceCount>) {
        for _ in r.read() {
            c.0 += 1;
        }
    }
    fn count_restart(mut r: MessageReader<RestartCombat>, mut c: ResMut<RestartCount>) {
        for _ in r.read() {
            c.0 += 1;
        }
    }

    fn make_db(on_defeat: OnDefeat) -> GameDb {
        use crate::content::content_view::ContentView;
        use crate::content::scenarios::{ScenarioDef, SceneDef};
        let enc = EncounterDef {
            id: "enc".into(),
            name: "enc".into(),
            enemies: vec![],
            victory: VictoryCondition::AllEnemiesDead,
            obstacles: vec![],
            environment: vec![],
            on_defeat,
            objectives: vec![],
        };
        let mut encounters = HashMap::new();
        encounters.insert("enc".into(), enc);
        let scen = ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party: vec![],
            scenes: vec![SceneDef::Combat {
                encounter_id: "enc".into(),
                location: None,
                on_victory_flags: vec![],
                requires_flag: None,
            }],
            content: ContentView::default(),
            encounters,
        };
        let mut db = GameDb {
            scenarios: HashMap::new(),
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };
        db.scenarios.insert("s1".into(), scen);
        db
    }

    fn base_app(on_defeat: OnDefeat, key: KeyCode) -> App {
        let mut app = App::new();
        app.add_message::<AdvanceScenario>();
        app.add_message::<RestartCombat>();
        app.init_resource::<AdvanceCount>();
        app.init_resource::<RestartCount>();
        app.insert_resource(make_db(on_defeat));
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        // Insert NextState directly — init_state requires StatesPlugin (DefaultPlugins).
        app.insert_resource(NextState::<crate::app_state::AppState>::default());
        let mut input = ButtonInput::<KeyCode>::default();
        input.press(key);
        app.insert_resource(input);
        app.add_systems(
            Update,
            (defeat_overlay_input, count_advance, count_restart).chain(),
        );
        app
    }

    /// Proceed + Space → AdvanceScenario written, no RestartCombat.
    #[test]
    fn proceed_space_writes_advance() {
        let mut app = base_app(OnDefeat::Proceed, KeyCode::Space);
        app.update();
        assert_eq!(
            app.world().resource::<AdvanceCount>().0,
            1,
            "expected one AdvanceScenario"
        );
        assert_eq!(
            app.world().resource::<RestartCount>().0,
            0,
            "expected no RestartCombat"
        );
    }

    /// Retry + R → RestartCombat written, no AdvanceScenario.
    #[test]
    fn retry_r_writes_restart() {
        let mut app = base_app(OnDefeat::Retry, KeyCode::KeyR);
        app.update();
        assert_eq!(
            app.world().resource::<RestartCount>().0,
            1,
            "expected one RestartCombat"
        );
        assert_eq!(
            app.world().resource::<AdvanceCount>().0,
            0,
            "expected no AdvanceScenario"
        );
    }
}
