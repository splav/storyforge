use bevy::prelude::*;

/// Controls how "sloppy" the enemy AI plays.
/// Scoring, armor awareness, heal urgency, etc. are balanced in the core
/// systems directly — only noise remains as a difficulty knob.
#[derive(Resource, Clone, Debug)]
pub struct DifficultyProfile {
    /// Random noise added to each candidate score (0 = deterministic, higher = sloppier).
    pub noise: f32,
}

impl DifficultyProfile {
    pub fn easy() -> Self {
        Self { noise: 0.5 }
    }

    pub fn normal() -> Self {
        Self { noise: 0.25 }
    }

    pub fn hard() -> Self {
        Self { noise: 0.0 }
    }
}

impl Default for DifficultyProfile {
    fn default() -> Self {
        Self::normal()
    }
}
