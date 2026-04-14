use super::AdvanceScenario;
use bevy::prelude::*;

pub fn victory_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    if keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) {
        writer.write(AdvanceScenario);
    }
}

