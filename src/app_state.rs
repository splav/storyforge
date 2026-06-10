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
    /// Between-story-scenes camp screen: player re-equips heroes from the stash.
    /// Entered automatically when advancing from a Story scene (no_camp=false) to
    /// another Story scene while CampaignState is present.
    Camp,
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
