use super::button::{spawn_standard_button, ButtonStyle};
use super::{ChoiceButton, StoryContinueButton, StoryScreenRoot};
use crate::content::scenarios::{line_visible, DialogueLine, SceneDef};
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::scenario::AdvanceScenario;
use bevy::prelude::*;

/// Tracks how many dialogue lines of the current story scene are revealed.
#[derive(Resource)]
pub struct StoryDialogue {
    pub lines: Vec<DialogueLine>,
    pub shown: usize,
}

/// Marker on the vertical container holding dialogue line rows.
#[derive(Component)]
pub struct StoryLinesColumn;

pub fn setup_story_screen(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    campaign: Option<Res<CampaignState>>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let all_lines = match &scen.scenes[scenario.scene_index] {
        SceneDef::Story { lines, .. } => lines,
        _ => return,
    };
    let empty_flags = std::collections::BTreeSet::new();
    let flags = campaign
        .as_deref()
        .map(|c| &c.flags)
        .unwrap_or(&empty_flags);
    let lines: Vec<DialogueLine> = all_lines
        .iter()
        .filter(|l| line_visible(l, flags))
        .cloned()
        .collect();
    assert!(
        !lines.is_empty(),
        "story scene has no visible dialogue lines"
    );

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    let is_last_scene = scenario.scene_index + 1 >= scen.scenes.len();
    let bg_path = if is_last_scene {
        "images/victory_background.png"
    } else {
        "images/story_background.png"
    };
    let bg_image: Handle<Image> = asset_server.load(bg_path);

    commands
        .spawn((
            StoryScreenRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            ImageNode {
                image: bg_image,
                ..default()
            },
            ZIndex(200),
        ))
        .with_children(|bg| {
            bg.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(45.0),
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(0.0),
                    left: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::FlexEnd,
                    align_items: AlignItems::Center,
                    padding: UiRect::all(Val::Px(16.0)),
                    row_gap: Val::Px(10.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.02, 0.05, 0.78)),
            ))
            .with_children(|root| {
                // Column that accumulates revealed dialogue lines.
                root.spawn((
                    StoryLinesColumn,
                    Node {
                        width: Val::Px(640.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(8.0),
                        ..default()
                    },
                ))
                .with_children(|col| {
                    spawn_line(col, &font, &lines[0]);
                });

                spawn_standard_button(
                    root,
                    font,
                    "Далее",
                    Val::Auto,
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(StoryContinueButton);
            });
        });

    commands.insert_resource(StoryDialogue { lines, shown: 1 });
}

fn spawn_line(parent: &mut ChildSpawnerCommands, font: &Handle<Font>, line: &DialogueLine) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Text::new(line.speaker.clone()),
                TextFont {
                    font: font.clone(),
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.82, 0.45)),
            ));
            row.spawn((
                Text::new(line.text.clone()),
                TextFont {
                    font: font.clone(),
                    font_size: 18.0,
                    ..default()
                },
                TextColor(Color::srgb(0.88, 0.88, 0.82)),
            ));
        });
}

pub fn story_input_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<StoryContinueButton>)>,
    dialogue: Option<ResMut<StoryDialogue>>,
    column: Query<Entity, With<StoryLinesColumn>>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    let key_pressed = keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter);
    let btn_clicked = buttons.iter().any(|i| *i == Interaction::Pressed);
    if !(key_pressed || btn_clicked) {
        return;
    }

    let Some(mut dialogue) = dialogue else {
        return;
    };

    if dialogue.shown < dialogue.lines.len() {
        if let Ok(col) = column.single() {
            let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
            let line = dialogue.lines[dialogue.shown].clone();
            commands.entity(col).with_children(|c| {
                spawn_line(c, &font, &line);
            });
            dialogue.shown += 1;
        }
    } else {
        writer.write(AdvanceScenario);
    }
}

pub fn setup_choice_screen(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    campaign: Option<Res<CampaignState>>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let (prompt, options) = match &scen.scenes[scenario.scene_index] {
        SceneDef::Choice {
            prompt, options, ..
        } => (prompt, options),
        _ => return,
    };
    let empty_flags = std::collections::BTreeSet::new();
    let flags = campaign
        .as_deref()
        .map(|c| &c.flags)
        .unwrap_or(&empty_flags);
    let visible_prompt: Vec<DialogueLine> = prompt
        .iter()
        .filter(|l| line_visible(l, flags))
        .cloned()
        .collect();

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    let is_last_scene = scenario.scene_index + 1 >= scen.scenes.len();
    let bg_path = if is_last_scene {
        "images/victory_background.png"
    } else {
        "images/story_background.png"
    };
    let bg_image: Handle<Image> = asset_server.load(bg_path);

    commands
        .spawn((
            StoryScreenRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            ImageNode {
                image: bg_image,
                ..default()
            },
            ZIndex(200),
        ))
        .with_children(|bg| {
            bg.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(45.0),
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(0.0),
                    left: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::FlexEnd,
                    align_items: AlignItems::Center,
                    padding: UiRect::all(Val::Px(16.0)),
                    row_gap: Val::Px(10.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.02, 0.05, 0.78)),
            ))
            .with_children(|root| {
                // Column holding all visible prompt lines (all shown at once).
                root.spawn((
                    StoryLinesColumn,
                    Node {
                        width: Val::Px(640.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(8.0),
                        ..default()
                    },
                ))
                .with_children(|col| {
                    for line in &visible_prompt {
                        spawn_line(col, &font, line);
                    }
                });

                // One button per choice option.
                for (i, option) in options.iter().enumerate() {
                    spawn_standard_button(
                        root,
                        font.clone(),
                        option.label.clone(),
                        Val::Auto,
                        Val::Auto,
                        ButtonStyle::Default,
                    )
                    .insert(ChoiceButton(i));
                }
            });
        });
    // No StoryDialogue resource — choices show all prompt lines at once.
}

pub fn choice_input_system(
    buttons: Query<(&Interaction, &ChoiceButton), Changed<Interaction>>,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    mut campaign: Option<ResMut<CampaignState>>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    for (interaction, ChoiceButton(idx)) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        let SceneDef::Choice { options, .. } = &scen.scenes[scenario.scene_index] else {
            return;
        };
        if let Some(opt) = options.get(*idx) {
            if let Some(campaign) = campaign.as_mut() {
                campaign.flags.insert(opt.set_flag.clone());
            }
            writer.write(AdvanceScenario);
        }
        return; // at most one choice per frame
    }
}

pub fn cleanup_story_screen(mut commands: Commands, roots: Query<Entity, With<StoryScreenRoot>>) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
    commands.remove_resource::<StoryDialogue>();
}
