use super::AdvanceScenario;
use crate::app_state::AppState;
use bevy::prelude::*;

pub fn victory_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    if keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) {
        writer.write(AdvanceScenario);
    }
}

pub fn defeat_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) {
        next_state.set(AppState::MainMenu);
    }
}
