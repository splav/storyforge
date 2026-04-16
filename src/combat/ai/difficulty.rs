use bevy::prelude::*;

/// Controls how "smart" the enemy AI plays.
/// All fields are continuous knobs — easy to interpolate for adaptive difficulty.
#[derive(Resource, Clone, Debug)]
pub struct DifficultyProfile {
    /// Score multiplier when expected damage ≥ target HP (kill opportunity).
    pub kill_multiplier: f32,
    /// Fraction of target armor factored into damage estimates (0 = ignore, 1 = full).
    pub armor_awareness: f32,
    /// Scale applied to status-effect value estimates.
    pub status_value_scale: f32,
    /// HP% threshold below which healing is considered urgent.
    pub heal_urgency_threshold: f32,
    /// Multiplier on heal score when target is below urgency threshold.
    pub heal_urgency_multiplier: f32,
    /// Random noise added to each score (0 = deterministic, higher = sloppier).
    pub noise: f32,
    /// How much of influence maps / spatial reasoning the AI uses (0 = ignores, 1 = full).
    pub awareness: f32,
}

impl DifficultyProfile {
    pub fn easy() -> Self {
        Self {
            kill_multiplier: 1.0,
            armor_awareness: 0.3,
            status_value_scale: 0.3,
            heal_urgency_threshold: 0.15,
            heal_urgency_multiplier: 1.2,
            noise: 3.0,
            awareness: 0.3,
        }
    }

    pub fn normal() -> Self {
        Self {
            kill_multiplier: 1.5,
            armor_awareness: 0.7,
            status_value_scale: 0.7,
            heal_urgency_threshold: 0.30,
            heal_urgency_multiplier: 1.5,
            noise: 1.0,
            awareness: 0.7,
        }
    }

    pub fn hard() -> Self {
        Self {
            kill_multiplier: 2.0,
            armor_awareness: 1.0,
            status_value_scale: 1.0,
            heal_urgency_threshold: 0.40,
            heal_urgency_multiplier: 1.8,
            noise: 0.0,
            awareness: 1.0,
        }
    }
}

impl Default for DifficultyProfile {
    fn default() -> Self {
        Self::normal()
    }
}
