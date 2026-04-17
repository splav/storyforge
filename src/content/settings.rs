use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::combat::ai::difficulty::DifficultyProfile;

#[derive(Resource, Clone)]
pub struct GameSettings {
    pub difficulty_preset: DifficultyPreset,
    pub difficulty: DifficultyProfile,
    pub crit_fail_die: u32,
    pub ai_debug: bool,
    pub current_slot: u8,
}

impl Default for GameSettings {
    fn default() -> Self {
        let preset = DifficultyPreset::Normal;
        Self {
            difficulty_preset: preset,
            difficulty: preset.profile(),
            crit_fail_die: 20,
            ai_debug: false,
            current_slot: 1,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DifficultyPreset {
    Easy,
    Normal,
    Hard,
}

impl DifficultyPreset {
    pub fn profile(self) -> DifficultyProfile {
        match self {
            Self::Easy => DifficultyProfile::easy(),
            Self::Normal => DifficultyProfile::normal(),
            Self::Hard => DifficultyProfile::hard(),
        }
    }
}

// ── TOML schema ───────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct SettingsFile {
    pub difficulty: DifficultySection,
    #[serde(default)]
    pub debug: DebugSection,
    #[serde(default)]
    pub profile: ProfileSection,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileSection {
    #[serde(default = "default_current_slot")]
    pub current_slot: u8,
}

impl Default for ProfileSection {
    fn default() -> Self {
        Self { current_slot: default_current_slot() }
    }
}

fn default_current_slot() -> u8 {
    1
}

#[derive(Serialize, Deserialize, Default)]
pub struct DebugSection {
    #[serde(default)]
    pub ai_debug: bool,
}

#[derive(Serialize, Deserialize)]
pub struct DifficultySection {
    pub ai: DifficultyPreset,
    #[serde(default = "default_crit_die")]
    pub crit_fail_die: u32,
}

fn default_crit_die() -> u32 {
    20
}

impl GameSettings {
    pub fn from_file(f: SettingsFile) -> Self {
        let preset = f.difficulty.ai;
        Self {
            difficulty_preset: preset,
            difficulty: preset.profile(),
            crit_fail_die: f.difficulty.crit_fail_die,
            ai_debug: f.debug.ai_debug,
            current_slot: clamp_slot(f.profile.current_slot),
        }
    }

    pub fn to_file(&self) -> SettingsFile {
        SettingsFile {
            difficulty: DifficultySection {
                ai: self.difficulty_preset,
                crit_fail_die: self.crit_fail_die,
            },
            debug: DebugSection { ai_debug: self.ai_debug },
            profile: ProfileSection { current_slot: self.current_slot },
        }
    }
}

fn clamp_slot(s: u8) -> u8 {
    s.clamp(1, crate::persistence::save_repo::SLOT_COUNT)
}

// ── Bundled defaults ──────────────────────────────────────────────────────────

const BUNDLED_SETTINGS_PATH: &str = "assets/data/settings.toml";

/// Load shipped defaults from the bundled asset TOML.
/// Panics if the file is missing or invalid — it is part of the build.
pub fn load_bundled_settings() -> GameSettings {
    let src = std::fs::read_to_string(BUNDLED_SETTINGS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {BUNDLED_SETTINGS_PATH}: {e}"));
    let file: SettingsFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {BUNDLED_SETTINGS_PATH}: {e}"));
    GameSettings::from_file(file)
}

/// Back-compat alias used by tests and callers that don't care about user overrides.
pub fn load_settings() -> GameSettings {
    load_bundled_settings()
}
