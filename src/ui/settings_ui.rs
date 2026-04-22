#![allow(clippy::too_many_arguments)]

use super::button::{spawn_standard_button, ButtonStyle};
use crate::app_state::AppState;
use crate::content::settings::{DifficultyPreset, GameSettings};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::persistence::{save_repo, settings_repo, PersistencePaths};
use crate::ui::modal::{PendingPrompt, PromptKind};
use bevy::prelude::*;

#[derive(Component)]
pub struct SettingsRoot;

#[derive(Component)]
pub struct DifficultyRadio(pub DifficultyPreset);

#[derive(Component)]
pub struct SlotRow(pub u8);

#[derive(Component)]
pub enum SlotAction {
    Switch(u8),
    Save(u8),
    Delete(u8),
}

#[derive(Component)]
pub struct BackButton;

pub fn setup_settings(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    settings: Res<GameSettings>,
    paths: Option<Res<PersistencePaths>>,
    db: Res<GameDb>,
) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    commands
        .spawn((
            SettingsRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(16.0),
                padding: UiRect::all(Val::Px(24.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.05, 0.08)),
            ZIndex(200),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("Настройки"),
                TextFont { font: font.clone(), font_size: 32.0, ..default() },
                TextColor(Color::srgb(0.85, 0.82, 0.70)),
                Node { margin: UiRect::bottom(Val::Px(20.0)), ..default() },
            ));

            // ── Difficulty ──────────────────────────────────────────────
            root.spawn((
                Text::new("Сложность"),
                TextFont { font: font.clone(), font_size: 18.0, ..default() },
                TextColor(Color::srgb(0.75, 0.72, 0.65)),
            ));
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(10.0),
                    margin: UiRect::bottom(Val::Px(24.0)),
                    ..default()
                },
            ))
            .with_children(|row| {
                for preset in [DifficultyPreset::Easy, DifficultyPreset::Normal, DifficultyPreset::Hard, DifficultyPreset::Epic] {
                    let is_active = settings.difficulty_preset == preset;
                    let label = format!(
                        "{}{}",
                        if is_active { "● " } else { "○ " },
                        match preset {
                            DifficultyPreset::Easy => "Легко",
                            DifficultyPreset::Normal => "Нормально",
                            DifficultyPreset::Hard => "Сложно",
                            DifficultyPreset::Epic => "Эпично",
                        }
                    );
                    spawn_standard_button(
                        row,
                        font.clone(),
                        label,
                        Val::Px(160.0),
                        Val::Auto,
                        ButtonStyle::Default,
                    )
                    .insert(DifficultyRadio(preset));
                }
            });

            // ── Slots ───────────────────────────────────────────────────
            root.spawn((
                Text::new("Слоты"),
                TextFont { font: font.clone(), font_size: 18.0, ..default() },
                TextColor(Color::srgb(0.75, 0.72, 0.65)),
            ));
            for slot in 1..=save_repo::SLOT_COUNT {
                spawn_slot_row(root, &font, &settings, paths.as_deref(), &db, slot);
            }

            // ── Back ────────────────────────────────────────────────────
            spawn_standard_button(
                root,
                font.clone(),
                "← Назад".to_string(),
                Val::Px(200.0),
                Val::Auto,
                ButtonStyle::Default,
            )
            .insert(BackButton);
        });
}

fn spawn_slot_row(
    parent: &mut ChildSpawnerCommands,
    font: &Handle<Font>,
    settings: &GameSettings,
    paths: Option<&PersistencePaths>,
    db: &GameDb,
    slot: u8,
) {
    let profile = paths.and_then(|p| save_repo::load(&p.0, slot));
    let is_active = settings.current_slot == slot;
    let is_empty = profile.is_none();

    let summary = profile.as_ref().and_then(|prof| {
        prof.last_campaign.as_ref().and_then(|id| {
            prof.campaigns.get(id).map(|pr| {
                let camp_name = db
                    .campaigns
                    .get(id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| id.clone());
                let scen_total = db
                    .campaigns
                    .get(id)
                    .map(|c| c.scenario_ids.len())
                    .unwrap_or(0);
                format!(
                    "{} — сценарий {}/{}",
                    camp_name,
                    pr.scenario_index + 1,
                    scen_total
                )
            })
        })
    });

    parent
        .spawn((
            SlotRow(slot),
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                padding: UiRect::all(Val::Px(8.0)),
                border: UiRect::all(Val::Px(1.0)),
                width: Val::Px(720.0),
                ..default()
            },
            BorderColor::all(if is_active {
                Color::srgb(0.75, 0.65, 0.30)
            } else {
                Color::srgb(0.25, 0.25, 0.28)
            }),
        ))
        .with_children(|row| {
            let badge = if is_active { " [активный]" } else { "" };
            let text = match &summary {
                Some(s) => format!("Слот {slot}{badge}: {s}"),
                None => format!("Слот {slot}{badge}: пусто"),
            };
            row.spawn((
                Text::new(text),
                TextFont { font: font.clone(), font_size: 15.0, ..default() },
                TextColor(Color::srgb(0.90, 0.88, 0.80)),
                Node { width: Val::Px(380.0), ..default() },
            ));

            if !is_active {
                spawn_standard_button(
                    row,
                    font.clone(),
                    "Переключиться".to_string(),
                    Val::Auto,
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(SlotAction::Switch(slot));
            }
            if is_active {
                spawn_standard_button(
                    row,
                    font.clone(),
                    "Сохранить".to_string(),
                    Val::Auto,
                    Val::Auto,
                    ButtonStyle::Default,
                )
                .insert(SlotAction::Save(slot));
            }
            if !is_empty {
                spawn_standard_button(
                    row,
                    font.clone(),
                    "Удалить".to_string(),
                    Val::Auto,
                    Val::Auto,
                    ButtonStyle::Danger,
                )
                .insert(SlotAction::Delete(slot));
            }
        });
}

pub fn difficulty_button_system(
    buttons: Query<(&Interaction, &DifficultyRadio), Changed<Interaction>>,
    mut settings: ResMut<GameSettings>,
    mut difficulty: ResMut<DifficultyProfile>,
    paths: Option<Res<PersistencePaths>>,
    mut rebuild: ResMut<SettingsRebuild>,
) {
    for (interaction, radio) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if settings.difficulty_preset == radio.0 {
            return;
        }
        settings.difficulty_preset = radio.0;
        settings.difficulty = radio.0.profile();
        *difficulty = settings.difficulty.clone();
        if let Some(p) = paths.as_deref() {
            if let Err(e) = settings_repo::save(&p.0, &settings) {
                warn!("settings save failed: {e}");
            }
        }
        rebuild.0 = true;
        return;
    }
}

pub fn slot_action_system(
    mut commands: Commands,
    buttons: Query<(&Interaction, &SlotAction), Changed<Interaction>>,
    mut prompt: ResMut<PendingPrompt>,
    paths: Option<Res<PersistencePaths>>,
    mut settings: ResMut<GameSettings>,
    campaign: Option<Res<CampaignState>>,
    scenario: Option<Res<ScenarioState>>,
    mut rebuild: ResMut<SettingsRebuild>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            SlotAction::Switch(target) => {
                if target == settings.current_slot {
                    return;
                }
                if campaign.is_some() && scenario.is_some() {
                    prompt.0 = Some(PromptKind::SwitchSlot { target });
                } else {
                    crate::ui::modal::switch_slot(
                        &mut commands,
                        &db,
                        &mut next_state,
                        paths.as_deref(),
                        &mut settings,
                        target,
                    );
                    rebuild.0 = true;
                }
            }
            SlotAction::Save(target) => {
                let Some(p) = paths.as_deref() else { return };
                let (Some(camp), Some(scen)) = (campaign.as_deref(), scenario.as_deref()) else {
                    warn!("no in-flight progress to save");
                    return;
                };
                if let Err(e) = save_repo::record_progress(
                    &p.0,
                    target,
                    &camp.campaign_id,
                    camp.scenario_index,
                    &scen.scenario_id,
                    scen.scene_index,
                ) {
                    warn!("manual save failed: {e}");
                }
                rebuild.0 = true;
            }
            SlotAction::Delete(target) => {
                if let Some(p) = paths.as_deref() {
                    if let Err(e) = save_repo::delete(&p.0, target) {
                        warn!("slot {target} delete failed: {e}");
                    }
                }
                rebuild.0 = true;
            }
        }
        return;
    }
}

pub fn back_button_system(
    buttons: Query<&Interaction, (Changed<Interaction>, With<BackButton>)>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    for i in &buttons {
        if *i == Interaction::Pressed {
            next_state.set(AppState::MainMenu);
            return;
        }
    }
}

/// Set to true by handlers that changed something the UI reflects; next frame the
/// settings screen is despawned and rebuilt.
#[derive(Resource, Default)]
pub struct SettingsRebuild(pub bool);

pub fn rebuild_settings_if_needed(
    mut commands: Commands,
    mut flag: ResMut<SettingsRebuild>,
    roots: Query<Entity, With<SettingsRoot>>,
    asset_server: Res<AssetServer>,
    settings: Res<GameSettings>,
    paths: Option<Res<PersistencePaths>>,
    db: Res<GameDb>,
    state: Res<State<AppState>>,
) {
    if !flag.0 {
        return;
    }
    flag.0 = false;
    if *state.get() != AppState::Settings {
        return;
    }
    for e in &roots {
        commands.entity(e).despawn();
    }
    setup_settings(commands, asset_server, settings, paths, db);
}

pub fn cleanup_settings(mut commands: Commands, roots: Query<Entity, With<SettingsRoot>>) {
    for e in &roots {
        commands.entity(e).despawn();
    }
}
