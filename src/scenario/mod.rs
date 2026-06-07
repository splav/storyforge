#![allow(clippy::too_many_arguments)]

pub mod combat_scene;
pub mod input;

use crate::app_state::AppState;
use crate::content::content_view::ActiveContent;
use crate::content::scenarios::SceneDef;
use crate::content::settings::GameSettings;
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::persistence::save_repo;
use crate::persistence::PersistencePaths;
use bevy::prelude::*;

#[derive(Message)]
pub struct AdvanceScenario;

pub fn start_scenario(mut commands: Commands, mut next_state: ResMut<NextState<AppState>>) {
    commands.spawn(Camera2d);
    next_state.set(AppState::MainMenu);
}

/// Initialize ScenarioState for the given scenario id at scene 0.
pub fn enter_scenario(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
) {
    enter_scenario_at(commands, db, next_state, scenario_id, 0);
}

pub fn enter_scenario_at(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
    scene_index: usize,
) {
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found"));
    assert!(
        scene_index < scen.scenes.len(),
        "scene_index {scene_index} out of range for scenario '{scenario_id}'"
    );
    let scene_index = skip_invisible(scen, scene_index).unwrap_or_else(|| {
        panic!("scenario '{scenario_id}' ends with only invisible scenes from index {scene_index}")
    });

    commands.insert_resource(ScenarioState {
        scenario_id: scenario_id.into(),
        scene_index,
    });
    // Publish the scenario's merged content view so combat systems see the
    // correct (possibly-overridden) abilities/statuses/weapons/etc.
    commands.insert_resource(ActiveContent(scen.content.clone()));

    // Build AI tag caches from the content view and insert them as Resources.
    // StatusTagCache is built first (no deps); AbilityTagCache uses it for
    // Defensive/ApplyCC/Peel classification.
    let (status_tags, ability_tags) =
        crate::combat::ai::world::tags::cache::build_caches(&scen.content);
    commands.insert_resource(status_tags);
    commands.insert_resource(ability_tags);

    match &scen.scenes[scene_index] {
        SceneDef::Story { .. } => next_state.set(AppState::Story),
        SceneDef::Combat { .. } => next_state.set(AppState::Combat),
    }
}

/// Walk forward past any invisible scenes (story with `lines = []`, used as a
/// pure party-change beat). Returns `None` if we ran off the end.
fn skip_invisible(scen: &crate::content::scenarios::ScenarioDef, mut idx: usize) -> Option<usize> {
    while idx < scen.scenes.len() {
        if scen.scenes[idx].is_invisible() {
            idx += 1;
            continue;
        }
        return Some(idx);
    }
    None
}

pub fn advance_scenario_system(
    mut commands: Commands,
    mut events: MessageReader<AdvanceScenario>,
    scenario: Option<ResMut<ScenarioState>>,
    campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
) {
    let Some(mut scenario) = scenario else { return };

    if events.read().next().is_none() {
        return;
    }
    events.clear();
    {
        scenario.scene_index += 1;

        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        // Skip invisible story beats (lines = [], used for party changes between visible scenes).
        match skip_invisible(scen, scenario.scene_index) {
            Some(idx) => scenario.scene_index = idx,
            None => {
                scenario.scene_index = scen.scenes.len();
            }
        }
        if scenario.scene_index >= scen.scenes.len() {
            // Scenario finished. Advance within campaign or end it.
            if let Some(mut camp_state) = campaign {
                camp_state.scenario_index += 1;
                let camp = db.campaigns.get(&camp_state.campaign_id).unwrap();
                if camp_state.scenario_index < camp.scenario_ids.len() {
                    let next_id = camp.scenario_ids[camp_state.scenario_index].clone();
                    enter_scenario(&mut commands, &db, &mut next_state, &next_id);
                    write_autosave(
                        paths.as_deref(),
                        settings.current_slot,
                        &camp_state,
                        &next_id,
                        0,
                    );
                    return;
                }
                // Campaign finished.
                let finished_id = camp_state.campaign_id.clone();
                commands.remove_resource::<CampaignState>();
                if let Some(p) = paths.as_deref() {
                    if let Err(e) =
                        save_repo::clear_campaign(&p.0, settings.current_slot, &finished_id)
                    {
                        warn!("failed to clear completed campaign from slot: {e}");
                    }
                }
            }
            next_state.set(AppState::MainMenu);
            return;
        }

        match &scen.scenes[scenario.scene_index] {
            SceneDef::Story { .. } => next_state.set(AppState::Story),
            SceneDef::Combat { .. } => next_state.set(AppState::Combat),
        }

        if let Some(camp) = campaign.as_deref() {
            write_autosave(
                paths.as_deref(),
                settings.current_slot,
                camp,
                &scenario.scenario_id,
                scenario.scene_index,
            );
        }
    }
}

fn write_autosave(
    paths: Option<&PersistencePaths>,
    slot: u8,
    campaign: &CampaignState,
    scenario_id: &str,
    scene_index: usize,
) {
    let Some(p) = paths else { return };
    if let Err(e) = save_repo::record_progress(&p.0, slot, campaign, scenario_id, scene_index) {
        warn!("autosave failed: {e}");
    }
}

/// On entering `CombatPhase::Victory`, collect `on_victory_flags` from the just-won
/// combat scene and insert them into `CampaignState.flags`.
///
/// Runs on `OnEnter(CombatPhase::Victory)` — fires *before* the player presses Space
/// to advance, so flags are already in `CampaignState` when `write_autosave` is called
/// from `advance_scenario_system`.
pub fn write_victory_flags(
    scenario: Option<Res<ScenarioState>>,
    mut campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
) {
    let (Some(scenario), Some(campaign)) = (scenario, campaign.as_mut()) else {
        return;
    };
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    if let SceneDef::Combat { on_victory_flags, .. } = &scen.scenes[scenario.scene_index] {
        for flag in on_victory_flags {
            campaign.flags.insert(flag.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;
    use crate::content::scenarios::ScenarioDef;
    use bevy::prelude::*;
    use std::collections::HashMap;

    fn minimal_scenario(id: &str, flags: Vec<&str>) -> ScenarioDef {
        ScenarioDef {
            id: id.into(),
            name: id.into(),
            party: vec![],
            scenes: vec![SceneDef::Combat {
                encounter_id: "enc".into(),
                location: None,
                on_victory_flags: flags.into_iter().map(str::to_string).collect(),
            }],
            content: ContentView::default(),
            encounters: HashMap::new(),
        }
    }

    fn make_db(scenario: ScenarioDef) -> GameDb {
        let mut db = GameDb {
            scenarios: HashMap::new(),
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };
        db.scenarios.insert(scenario.id.clone(), scenario);
        db
    }

    /// `write_victory_flags` inserts all `on_victory_flags` of the current
    /// combat scene into `CampaignState.flags`.
    #[test]
    fn victory_flags_written_to_campaign_state() {
        let scenario = minimal_scenario("s1", vec!["found_token", "kael_found"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.add_systems(Update, write_victory_flags);
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.contains("found_token"), "found_token should be in flags");
        assert!(flags.contains("kael_found"), "kael_found should be in flags");
        assert_eq!(flags.len(), 2);
    }

    /// `write_victory_flags` is idempotent — running twice yields the same set.
    #[test]
    fn victory_flags_idempotent() {
        let scenario = minimal_scenario("s1", vec!["flag_x"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.add_systems(Update, write_victory_flags);
        app.update();
        app.update(); // second call — must not duplicate

        let flags = &app.world().resource::<CampaignState>().flags;
        assert_eq!(flags.len(), 1);
    }

    /// `write_victory_flags` is a no-op when `CampaignState` is absent
    /// (e.g., standalone scenario run without a campaign wrapper).
    #[test]
    fn victory_flags_no_campaign_state_is_noop() {
        let scenario = minimal_scenario("s1", vec!["flag_a"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        // No CampaignState inserted — must not panic.
        app.add_systems(Update, write_victory_flags);
        app.update();
    }
}
