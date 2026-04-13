use super::{StoryContinueButton, StoryScreenRoot};
use crate::content::scenarios::SceneDef;
use crate::game::resources::{GameDb, ScenarioState};
use crate::scenario::AdvanceScenario;
use bevy::prelude::*;

pub fn setup_story_screen(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let text = match &scen.scenes[scenario.scene_index] {
        SceneDef::Story { text } => text.clone(),
        _ => return,
    };

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    commands
        .spawn((
            StoryScreenRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(40.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.05, 0.08)),
            ZIndex(200),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new(text),
                TextFont {
                    font: font.clone(),
                    font_size: 18.0,
                    ..default()
                },
                TextColor(Color::srgb(0.85, 0.85, 0.80)),
                Node {
                    max_width: Val::Px(600.0),
                    margin: UiRect::bottom(Val::Px(40.0)),
                    ..default()
                },
            ));

            root.spawn((
                StoryContinueButton,
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(24.0), Val::Px(12.0)),
                    border: UiRect::all(Val::Px(1.5)),
                    ..default()
                },
                BorderColor::all(Color::srgb(0.4, 0.4, 0.3)),
                BackgroundColor(Color::srgb(0.12, 0.12, 0.10)),
            ))
            .with_children(|btn| {
                btn.spawn((
                    Text::new("Продолжить"),
                    TextFont {
                        font,
                        font_size: 16.0,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            });
        });
}

pub fn story_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<StoryContinueButton>)>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    let key_pressed = keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter);
    let btn_clicked = buttons.iter().any(|i| *i == Interaction::Pressed);
    if key_pressed || btn_clicked {
        writer.write(AdvanceScenario);
    }
}

pub fn cleanup_story_screen(mut commands: Commands, roots: Query<Entity, With<StoryScreenRoot>>) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
}
