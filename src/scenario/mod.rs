pub mod combat_scene;
pub mod input;

use crate::app_state::AppState;
use crate::content::scenarios::SceneDef;
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use bevy::prelude::*;

#[derive(Message)]
pub struct AdvanceScenario;

pub fn start_scenario(mut commands: Commands, mut next_state: ResMut<NextState<AppState>>) {
    commands.spawn(Camera2d);
    next_state.set(AppState::MainMenu);
}

/// Initialize ScenarioState for the given scenario id and set AppState
/// to the correct screen for its first scene.
pub fn enter_scenario(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
) {
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found"));

    commands.insert_resource(ScenarioState {
        scenario_id: scenario_id.into(),
        scene_index: 0,
    });

    match &scen.scenes[0] {
        SceneDef::Story { .. } => next_state.set(AppState::Story),
        SceneDef::Combat { .. } => next_state.set(AppState::Combat),
    }
}

pub fn advance_scenario_system(
    mut commands: Commands,
    mut events: MessageReader<AdvanceScenario>,
    scenario: Option<ResMut<ScenarioState>>,
    campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    let Some(mut scenario) = scenario else { return };

    for _ in events.read() {
        scenario.scene_index += 1;

        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        if scenario.scene_index >= scen.scenes.len() {
            // Scenario finished. Try to advance within the active campaign.
            if let Some(mut camp_state) = campaign {
                camp_state.scenario_index += 1;
                let camp = db.campaigns.get(&camp_state.campaign_id).unwrap();
                if camp_state.scenario_index < camp.scenario_ids.len() {
                    let next_id = camp.scenario_ids[camp_state.scenario_index].clone();
                    enter_scenario(&mut commands, &db, &mut next_state, &next_id);
                    return;
                }
                // Campaign finished — drop its state and return to menu.
                commands.remove_resource::<CampaignState>();
            }
            next_state.set(AppState::MainMenu);
            return;
        }

        match &scen.scenes[scenario.scene_index] {
            SceneDef::Story { .. } => next_state.set(AppState::Story),
            SceneDef::Combat { .. } => next_state.set(AppState::Combat),
        }
    }
}
