use bevy::prelude::*;

#[derive(States, Default, Debug, Clone, Eq, PartialEq, Hash)]
pub enum AppState {
    #[default]
    Boot,
    MainMenu,
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
    ResolveAction,
    EnemyTurn,
    Cleanup,
    Victory,
    Defeat,
}
