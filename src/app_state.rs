use bevy::prelude::*;

#[derive(States, Default, Debug, Clone, Eq, PartialEq, Hash)]
pub enum AppState {
    #[default]
    Boot,
    MainMenu,
    Settings,
    Story,
    Overworld,
    Combat,
}

/// SubState, active only when AppState == Combat.
#[derive(SubStates, Default, Debug, Clone, Eq, PartialEq, Hash)]
#[source(AppState = AppState::Combat)]
pub enum CombatPhase {
    #[default]
    StartRound,
    AwaitCommand,
    Victory,
    Defeat,
}
