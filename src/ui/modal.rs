use super::button::{spawn_standard_button, ButtonStyle};
use super::UiFont;
use crate::app_state::AppState;
use crate::content::settings::GameSettings;
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::persistence::save_repo::{self, CampaignProgress};
use crate::persistence::PersistencePaths;
use crate::ui::main_menu_ui::{start_campaign_fresh, validate_and_resume};
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct PendingPrompt(pub Option<PromptKind>);

#[derive(Clone, Debug)]
pub enum PromptKind {
    /// Picked a campaign from main menu that already has progress in the active slot.
    CampaignHasProgress {
        campaign_id: String,
        progress: CampaignProgress,
    },
    /// Switching active slot while in-flight progress exists.
    SwitchSlot {
        target: u8,
    },
}

#[derive(Component)]
pub struct ModalRoot;

#[derive(Component)]
pub struct ModalChoice(pub usize);

/// Spawn/despawn the modal UI whenever PendingPrompt changes.
pub fn sync_modal(
    mut commands: Commands,
    prompt: Res<PendingPrompt>,
    font: Option<Res<UiFont>>,
    roots: Query<Entity, With<ModalRoot>>,
    asset_server: Res<AssetServer>,
) {
    if !prompt.is_changed() {
        return;
    }
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(kind) = prompt.0.as_ref() else { return };

    let font_handle: Handle<Font> = font
        .map(|f| f.0.clone())
        .unwrap_or_else(|| asset_server.load("fonts/unicode.ttf"));

    let (title, choices) = match kind {
        PromptKind::CampaignHasProgress { .. } => (
            "В этом слоте уже есть прогресс по кампании.".to_string(),
            vec!["Продолжить", "Начать заново", "Отмена"],
        ),
        PromptKind::SwitchSlot { target } => (
            format!("Сохранить прогресс в текущий слот перед переходом на слот {target}?"),
            vec!["Сохранить и переключить", "Переключить без сохранения", "Отмена"],
        ),
    };

    commands
        .spawn((
            ModalRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.75)),
            ZIndex(1000),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new(title),
                TextFont { font: font_handle.clone(), font_size: 20.0, ..default() },
                TextColor(Color::srgb(0.90, 0.88, 0.80)),
                Node { margin: UiRect::bottom(Val::Px(12.0)), ..default() },
            ));
            for (i, label) in choices.iter().enumerate() {
                spawn_standard_button(
                    root,
                    font_handle.clone(),
                    (*label).to_string(),
                    Val::Px(360.0),
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(ModalChoice(i));
            }
        });
}

/// Dispatch modal button presses to the right handler based on current prompt kind.
#[allow(clippy::too_many_arguments)]
pub fn handle_modal_input(
    mut commands: Commands,
    buttons: Query<(&Interaction, &ModalChoice), Changed<Interaction>>,
    mut prompt: ResMut<PendingPrompt>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
    paths: Option<Res<PersistencePaths>>,
    mut settings: ResMut<GameSettings>,
    campaign: Option<Res<CampaignState>>,
    scenario: Option<Res<ScenarioState>>,
) {
    for (interaction, choice) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(kind) = prompt.0.clone() else { return };
        match kind {
            PromptKind::CampaignHasProgress { campaign_id, progress } => match choice.0 {
                0 => {
                    if !validate_and_resume(
                        &mut commands,
                        &db,
                        &mut next_state,
                        &campaign_id,
                        &progress,
                    ) {
                        warn!("stale progress — starting fresh");
                        start_campaign_fresh(
                        &mut commands,
                        &db,
                        &mut next_state,
                        &campaign_id,
                        paths.as_deref(),
                        settings.current_slot,
                    );
                    }
                    prompt.0 = None;
                }
                1 => {
                    if let Some(p) = paths.as_deref() {
                        let _ = save_repo::clear_campaign(
                            &p.0,
                            settings.current_slot,
                            &campaign_id,
                        );
                    }
                    start_campaign_fresh(
                        &mut commands,
                        &db,
                        &mut next_state,
                        &campaign_id,
                        paths.as_deref(),
                        settings.current_slot,
                    );
                    prompt.0 = None;
                }
                _ => prompt.0 = None,
            },
            PromptKind::SwitchSlot { target } => match choice.0 {
                0 => {
                    // Save current progress into old slot first.
                    if let (Some(p), Some(camp), Some(scen)) =
                        (paths.as_deref(), campaign.as_deref(), scenario.as_deref())
                    {
                        if let Err(e) = save_repo::record_progress(
                            &p.0,
                            settings.current_slot,
                            camp,
                            &scen.scenario_id,
                            scen.scene_index,
                        ) {
                            warn!("pre-switch save failed: {e}");
                        }
                    }
                    switch_slot(
                        &mut commands,
                        &db,
                        &mut next_state,
                        paths.as_deref(),
                        &mut settings,
                        target,
                    );
                    prompt.0 = None;
                }
                1 => {
                    switch_slot(
                        &mut commands,
                        &db,
                        &mut next_state,
                        paths.as_deref(),
                        &mut settings,
                        target,
                    );
                    prompt.0 = None;
                }
                _ => prompt.0 = None,
            },
        }
        return;
    }
}

/// Perform the slot switch: drop in-memory progress, persist `current_slot`,
/// then either load last campaign from new slot or return to menu if empty.
pub fn switch_slot(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    paths: Option<&PersistencePaths>,
    settings: &mut GameSettings,
    target: u8,
) {
    settings.current_slot = target;
    commands.remove_resource::<CampaignState>();
    commands.remove_resource::<ScenarioState>();

    if let Some(p) = paths {
        if let Err(e) = crate::persistence::settings_repo::save(&p.0, settings) {
            warn!("failed to persist current_slot: {e}");
        }
    }

    // Try to resume last campaign in the new slot.
    let resumed = paths
        .and_then(|p| save_repo::load(&p.0, target))
        .and_then(|profile| {
            profile
                .last_campaign
                .clone()
                .and_then(|id| profile.campaigns.get(&id).cloned().map(|pr| (id, pr)))
        });

    if let Some((id, pr)) = resumed {
        if validate_and_resume(commands, db, next_state, &id, &pr) {
            return;
        }
        warn!("new slot's last campaign is stale; returning to menu");
    }
    next_state.set(AppState::MainMenu);
}
