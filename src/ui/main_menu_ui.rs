use super::button::{spawn_standard_button, ButtonStyle};
use super::{CampaignButton, MainMenuRoot};
use crate::app_state::AppState;
use crate::game::resources::{CampaignState, GameDb};
use crate::scenario::enter_scenario;
use bevy::prelude::*;

pub fn setup_main_menu(mut commands: Commands, asset_server: Res<AssetServer>, db: Res<GameDb>) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    commands
        .spawn((
            MainMenuRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.05, 0.08)),
            ZIndex(200),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("Storyforge"),
                TextFont {
                    font: font.clone(),
                    font_size: 36.0,
                    ..default()
                },
                TextColor(Color::srgb(0.85, 0.82, 0.70)),
                Node {
                    margin: UiRect::bottom(Val::Px(24.0)),
                    ..default()
                },
            ));

            for id in &db.campaign_order {
                let Some(camp) = db.campaigns.get(id) else { continue };
                spawn_standard_button(
                    root,
                    font.clone(),
                    camp.name.clone(),
                    Val::Px(320.0),
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(CampaignButton(camp.id.clone()));
            }
        });
}

pub fn campaign_button_system(
    mut commands: Commands,
    buttons: Query<(&Interaction, &CampaignButton), Changed<Interaction>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let camp = db
            .campaigns
            .get(&button.0)
            .unwrap_or_else(|| panic!("Campaign '{}' not found", button.0));
        let first_scenario = camp.scenario_ids[0].clone();
        commands.insert_resource(CampaignState {
            campaign_id: camp.id.clone(),
            scenario_index: 0,
        });
        enter_scenario(&mut commands, &db, &mut next_state, &first_scenario);
        return;
    }
}

pub fn cleanup_main_menu(mut commands: Commands, roots: Query<Entity, With<MainMenuRoot>>) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
}
