use super::button::{spawn_standard_button, ButtonStyle};
use super::{CampaignButton, MainMenuRoot};
use crate::app_state::AppState;
use crate::content::settings::GameSettings;
use crate::game::resources::{CampaignState, GameDb};
use crate::persistence::save_repo::{self, CampaignProgress};
use crate::persistence::PersistencePaths;
use crate::scenario::{enter_scenario, enter_scenario_at};
use crate::ui::modal::{PendingPrompt, PromptKind};
use bevy::prelude::*;

/// "Продолжить" — resumes last_campaign of the active slot.
#[derive(Component)]
pub struct ContinueButton;

/// "Настройки" — opens the settings screen.
#[derive(Component)]
pub struct SettingsButton;

pub fn setup_main_menu(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    let resume_campaign = paths
        .as_deref()
        .and_then(|p| save_repo::load(&p.0, settings.current_slot))
        .and_then(|prof| prof.last_campaign.clone().map(|id| (id, prof)));

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

            root.spawn((
                Text::new(format!("Слот: {}", settings.current_slot)),
                TextFont { font: font.clone(), font_size: 16.0, ..default() },
                TextColor(Color::srgb(0.60, 0.60, 0.65)),
                Node { margin: UiRect::bottom(Val::Px(8.0)), ..default() },
            ));

            if let Some((id, _)) = &resume_campaign {
                let label = db
                    .campaigns
                    .get(id)
                    .map(|c| format!("Продолжить: {}", c.name))
                    .unwrap_or_else(|| "Продолжить".to_string());
                spawn_standard_button(
                    root,
                    font.clone(),
                    label,
                    Val::Px(320.0),
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(ContinueButton);
            }

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

            spawn_standard_button(
                root,
                font.clone(),
                "Настройки".to_string(),
                Val::Px(320.0),
                Val::Auto,
                ButtonStyle::Default,
            )
            .insert(SettingsButton);
        });
}

pub fn campaign_button_system(
    mut commands: Commands,
    buttons: Query<(&Interaction, &CampaignButton), Changed<Interaction>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
    mut prompt: ResMut<PendingPrompt>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let camp = db
            .campaigns
            .get(&button.0)
            .unwrap_or_else(|| panic!("Campaign '{}' not found", button.0));

        // Check existing progress for this campaign in the active slot.
        let existing = paths
            .as_deref()
            .and_then(|p| save_repo::load(&p.0, settings.current_slot))
            .and_then(|prof| prof.campaigns.get(&camp.id).cloned());

        if let Some(progress) = existing {
            prompt.0 = Some(PromptKind::CampaignHasProgress {
                campaign_id: camp.id.clone(),
                progress,
            });
            return;
        }

        start_campaign_fresh(
            &mut commands,
            &db,
            &mut next_state,
            &camp.id,
            paths.as_deref(),
            settings.current_slot,
        );
        return;
    }
}

pub fn continue_button_system(
    mut commands: Commands,
    buttons: Query<&Interaction, (Changed<Interaction>, With<ContinueButton>)>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
) {
    for interaction in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(p) = paths.as_deref() else { return };
        let Some(profile) = save_repo::load(&p.0, settings.current_slot) else {
            return;
        };
        let Some(last_id) = profile.last_campaign else { return };
        let Some(progress) = profile.campaigns.get(&last_id) else { return };

        if !validate_and_resume(&mut commands, &db, &mut next_state, &last_id, progress) {
            warn!("continue: stale progress, discarded");
            let _ = save_repo::clear_campaign(&p.0, settings.current_slot, &last_id);
        }
        return;
    }
}

pub fn settings_button_system(
    buttons: Query<&Interaction, (Changed<Interaction>, With<SettingsButton>)>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    for interaction in &buttons {
        if *interaction == Interaction::Pressed {
            next_state.set(AppState::Settings);
            return;
        }
    }
}

pub fn cleanup_main_menu(mut commands: Commands, roots: Query<Entity, With<MainMenuRoot>>) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
}

// ── Helpers used by main menu and prompt handlers ───────────────────────────

pub fn start_campaign_fresh(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    campaign_id: &str,
    paths: Option<&PersistencePaths>,
    slot: u8,
) {
    let camp = db
        .campaigns
        .get(campaign_id)
        .unwrap_or_else(|| panic!("Campaign '{campaign_id}' not found"));
    let first_scenario = camp.scenario_ids[0].clone();
    let campaign_state = CampaignState {
        campaign_id: camp.id.clone(),
        scenario_index: 0,
        flags: Default::default(),
    };
    commands.insert_resource(campaign_state.clone());
    // Fresh campaign has no flags yet; pass empty set so flag-gated scenes at index 0 skip.
    enter_scenario(commands, db, next_state, &first_scenario, Some(&campaign_state.flags));
    if let Some(p) = paths {
        if let Err(e) = save_repo::record_progress(&p.0, slot, &campaign_state, &first_scenario, 0)
        {
            warn!("autosave on new game failed: {e}");
        }
    }
}

/// Returns false if progress refers to content that no longer exists.
pub fn validate_and_resume(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    campaign_id: &str,
    progress: &CampaignProgress,
) -> bool {
    let Some(camp) = db.campaigns.get(campaign_id) else { return false };
    if progress.scenario_index >= camp.scenario_ids.len() {
        return false;
    }
    let Some(scen) = db.scenarios.get(&progress.scenario_id) else { return false };
    if progress.scene_index >= scen.scenes.len() {
        return false;
    }
    let flags: std::collections::BTreeSet<String> =
        progress.flags.iter().cloned().collect();
    commands.insert_resource(CampaignState {
        campaign_id: campaign_id.to_string(),
        scenario_index: progress.scenario_index,
        flags: flags.clone(),
    });
    enter_scenario_at(
        commands,
        db,
        next_state,
        &progress.scenario_id,
        progress.scene_index,
        Some(&flags),
    );
    true
}
