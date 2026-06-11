use super::button::{spawn_standard_button, ButtonStyle};
use super::{CampaignButton, MainMenuRoot};
use crate::app_state::AppState;
use crate::content::scenarios::ScenarioDef;
#[cfg(feature = "dev")]
use crate::content::scenarios::SceneDef;
use crate::content::settings::GameSettings;
use crate::game::resources::{CampaignState, GameDb};
use crate::persistence::save_repo::{self, CampaignProgress};
use crate::persistence::PersistencePaths;
use crate::scenario::enter_scenario_at;
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
                TextFont {
                    font: font.clone(),
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.60, 0.60, 0.65)),
                Node {
                    margin: UiRect::bottom(Val::Px(8.0)),
                    ..default()
                },
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
                let Some(camp) = db.campaigns.get(id) else {
                    continue;
                };
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
            &settings,
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
        let Some(last_id) = profile.last_campaign else {
            return;
        };
        let Some(progress) = profile.campaigns.get(&last_id) else {
            return;
        };

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

/// Resolve which scenario index to start at for a fresh campaign.
///
/// In dev builds, a non-empty `dev_start` is looked up in `scenario_ids`.
/// Falls back to index 0 on mismatch or when the field is empty.
/// In non-dev builds this always returns 0 — the override block is compiled out.
pub fn resolve_start_index(scenario_ids: &[String], _dev_start: &str) -> usize {
    #[cfg(feature = "dev")]
    if !_dev_start.is_empty() {
        if let Some(idx) = scenario_ids.iter().position(|id| id == _dev_start) {
            return idx;
        }
        warn!(
            "[dev] start_scenario '{}' not found in campaign — starting at chapter 0",
            _dev_start
        );
    }
    let _ = scenario_ids; // suppress unused warning in non-dev builds
    0
}

/// Resolve which scene index within a scenario to start at for a fresh campaign.
///
/// In dev builds, a non-empty `dev_start_scene` is matched against `SceneDef::Combat`
/// encounter ids. Falls back to index 0 on mismatch or when the field is empty.
/// In non-dev builds this always returns 0 — the override block is compiled out.
pub fn resolve_start_scene_index(scen: &ScenarioDef, _dev_start_scene: &str) -> usize {
    #[cfg(feature = "dev")]
    if !_dev_start_scene.is_empty() {
        if let Some(idx) = scen.scenes.iter().position(|s| {
            matches!(s, SceneDef::Combat { encounter_id, .. } if encounter_id == _dev_start_scene)
        }) {
            return idx;
        }
        warn!(
            "[dev] start_scene '{}' not found in scenario '{}' — starting at scene 0",
            _dev_start_scene, scen.id
        );
    }
    let _ = scen; // suppress unused warning in non-dev builds
    0
}

pub fn start_campaign_fresh(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    campaign_id: &str,
    paths: Option<&PersistencePaths>,
    slot: u8,
    #[cfg_attr(not(feature = "dev"), allow(unused_variables))] settings: &GameSettings,
) {
    let camp = db
        .campaigns
        .get(campaign_id)
        .unwrap_or_else(|| panic!("Campaign '{campaign_id}' not found"));

    let start_index = resolve_start_index(&camp.scenario_ids, &settings.dev_start_scenario);
    let start_scenario = camp.scenario_ids[start_index].clone();

    #[cfg(feature = "dev")]
    if start_index > 0 {
        info!(
            "[dev] starting campaign '{}' at chapter '{}' (index {})",
            campaign_id, start_scenario, start_index
        );
    }

    let scen = db
        .scenarios
        .get(&start_scenario)
        .unwrap_or_else(|| panic!("Scenario '{start_scenario}' not found"));
    let start_scene_index = resolve_start_scene_index(scen, &settings.dev_start_scene);

    #[cfg(feature = "dev")]
    if start_scene_index > 0 {
        info!(
            "[dev] starting scenario '{}' at scene index {} (encounter-jump)",
            start_scenario, start_scene_index
        );
    }

    // Build the initial stash — normally empty; in dev+start_in_camp mode seed
    // a variety of test items so the camp screen has swappable content.
    #[cfg(feature = "dev")]
    let initial_stash: Vec<crate::content::item_ref::ItemRef> = if settings.dev_start_in_camp {
        use crate::content::item_ref::ItemRef;
        use combat_engine::{ArmorId, WeaponId};
        vec![
            ItemRef::Weapon(WeaponId::from("kolm_cleaver")),
            ItemRef::Weapon(WeaponId::from("short_sword")),
            ItemRef::Armor(ArmorId::from("warded_jerkin")),
            ItemRef::Armor(ArmorId::from("chainmail")),
            ItemRef::Armor(ArmorId::from("iron_boots")),
            ItemRef::Armor(ArmorId::from("plate_greaves")),
        ]
    } else {
        Vec::new()
    };
    #[cfg(not(feature = "dev"))]
    let initial_stash: Vec<crate::content::item_ref::ItemRef> = Vec::new();

    let campaign_state = CampaignState {
        campaign_id: camp.id.clone(),
        scenario_index: start_index,
        flags: Default::default(),
        stash: initial_stash,
        loadouts: std::collections::HashMap::new(),
    };
    commands.insert_resource(campaign_state.clone());

    // Fresh campaign has no flags yet; pass empty set so flag-gated scenes skip.
    // Note: story scenes before start_scene_index are not played, so their flags
    // won't be set — acceptable for a dev jump.
    enter_scenario_at(
        commands,
        db,
        next_state,
        &start_scenario,
        start_scene_index,
        Some(&campaign_state.flags),
    );

    // Dev: override next_state to Camp so the player lands in the equip screen
    // with the seeded stash. enter_scenario_at already set up ScenarioState and
    // ActiveContent; the camp Continue button will push on to AppState::Story.
    #[cfg(feature = "dev")]
    if settings.dev_start_in_camp {
        info!("[dev] start_in_camp=true — entering AppState::Camp with seeded stash");
        next_state.set(AppState::Camp);
    }

    if let Some(p) = paths {
        if let Err(e) = save_repo::record_progress(
            &p.0,
            slot,
            &campaign_state,
            &start_scenario,
            start_scene_index,
        ) {
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
    let Some(camp) = db.campaigns.get(campaign_id) else {
        return false;
    };
    if progress.scenario_index >= camp.scenario_ids.len() {
        return false;
    }
    let Some(scen) = db.scenarios.get(&progress.scenario_id) else {
        return false;
    };
    if progress.scene_index >= scen.scenes.len() {
        return false;
    }
    let flags: std::collections::BTreeSet<String> = progress.flags.iter().cloned().collect();
    commands.insert_resource(CampaignState {
        campaign_id: campaign_id.to_string(),
        scenario_index: progress.scenario_index,
        flags: flags.clone(),
        stash: progress.stash.clone(),
        loadouts: progress.loadouts.clone(),
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

#[cfg(test)]
mod tests {
    use super::{resolve_start_index, resolve_start_scene_index};
    use crate::content::content_view::ContentView;
    use crate::content::scenarios::{ScenarioDef, SceneDef};
    use std::collections::HashMap;

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Build a minimal ScenarioDef with the given id and scenes.
    fn scenario(id: &str, scenes: Vec<SceneDef>) -> ScenarioDef {
        ScenarioDef {
            id: id.into(),
            name: id.into(),
            party: vec![],
            scenes,
            content: ContentView::default(),
            encounters: HashMap::new(),
        }
    }

    fn combat(encounter_id: &str) -> SceneDef {
        SceneDef::Combat {
            encounter_id: encounter_id.into(),
            location: None,
            on_victory_flags: vec![],
            requires_flag: None,
        }
    }

    fn story() -> SceneDef {
        SceneDef::Story {
            lines: vec![],
            party_add: vec![],
            party_remove: vec![],
            status_ops: vec![],
            requires_flag: None,
            no_camp: false,
        }
    }

    // ── resolve_start_index ──────────────────────────────────────────────────

    #[test]
    fn empty_dev_start_always_returns_zero() {
        let campaign = ids(&["ch1", "ch2", "ch3"]);
        assert_eq!(resolve_start_index(&campaign, ""), 0);
    }

    #[cfg(feature = "dev")]
    #[test]
    fn dev_known_id_returns_correct_index() {
        let campaign = ids(&["ch1", "ch2", "ch3"]);
        assert_eq!(resolve_start_index(&campaign, "ch3"), 2);
        assert_eq!(resolve_start_index(&campaign, "ch2"), 1);
        assert_eq!(resolve_start_index(&campaign, "ch1"), 0);
    }

    #[cfg(feature = "dev")]
    #[test]
    fn dev_unknown_id_falls_back_to_zero() {
        let campaign = ids(&["ch1", "ch2", "ch3"]);
        assert_eq!(resolve_start_index(&campaign, "ch99"), 0);
    }

    // Non-dev path: override is compiled out, always index 0 regardless of value.
    #[cfg(not(feature = "dev"))]
    #[test]
    fn non_dev_ignores_nonempty_string() {
        let campaign = ids(&["ch1", "ch2", "ch3"]);
        assert_eq!(resolve_start_index(&campaign, "ch3"), 0);
    }

    // ── resolve_start_scene_index ────────────────────────────────────────────

    #[test]
    fn empty_scene_start_always_returns_zero() {
        let scen = scenario("ch1", vec![story(), combat("boss"), story()]);
        assert_eq!(resolve_start_scene_index(&scen, ""), 0);
    }

    #[cfg(feature = "dev")]
    #[test]
    fn dev_scene_known_encounter_returns_correct_index() {
        // story(0), combat("intro",1), story(2), combat("boss",3)
        let scen = scenario(
            "ch1",
            vec![story(), combat("intro"), story(), combat("boss")],
        );
        assert_eq!(resolve_start_scene_index(&scen, "boss"), 3);
        assert_eq!(resolve_start_scene_index(&scen, "intro"), 1);
    }

    #[cfg(feature = "dev")]
    #[test]
    fn dev_scene_unknown_encounter_falls_back_to_zero() {
        let scen = scenario("ch1", vec![story(), combat("boss")]);
        assert_eq!(resolve_start_scene_index(&scen, "no_such_enc"), 0);
    }

    // Non-dev path: override compiled out, always 0 regardless of value.
    #[cfg(not(feature = "dev"))]
    #[test]
    fn non_dev_scene_ignores_nonempty_string() {
        let scen = scenario("ch1", vec![story(), combat("boss")]);
        assert_eq!(resolve_start_scene_index(&scen, "boss"), 0);
    }
}
