pub mod combat_scene;
pub mod input;

use crate::app_state::AppState;
use crate::content::scenarios::SceneDef;
use crate::game::resources::{GameDb, ScenarioState};
use bevy::prelude::*;

#[derive(Message)]
pub struct AdvanceScenario;

pub fn start_scenario(
    mut commands: Commands,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    commands.spawn(Camera2d);

    let scenario_id = "demo";
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found in scenarios.toml"));

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
    mut events: MessageReader<AdvanceScenario>,
    scenario: Option<ResMut<ScenarioState>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    let Some(mut scenario) = scenario else { return };

    for _ in events.read() {
        scenario.scene_index += 1;

        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        if scenario.scene_index >= scen.scenes.len() {
            next_state.set(AppState::MainMenu);
            return;
        }

        match &scen.scenes[scenario.scene_index] {
            SceneDef::Story { .. } => next_state.set(AppState::Story),
            SceneDef::Combat { .. } => next_state.set(AppState::Combat),
        }
    }
}
