use bevy::prelude::*;
use serde::Deserialize;

use crate::combat::ai_difficulty::DifficultyProfile;

#[derive(Resource, Clone)]
pub struct GameSettings {
    pub difficulty: DifficultyProfile,
    pub crit_fail_die: u32,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            difficulty: DifficultyProfile::default(),
            crit_fail_die: 20,
        }
    }
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SettingsFile {
    difficulty: DifficultySection,
}

#[derive(Deserialize)]
struct DifficultySection {
    ai: String,
    #[serde(default = "default_crit_die")]
    crit_fail_die: u32,
}

fn default_crit_die() -> u32 {
    20
}

const SETTINGS_PATH: &str = "assets/data/settings.toml";

pub fn load_settings() -> GameSettings {
    let src = std::fs::read_to_string(SETTINGS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {SETTINGS_PATH}: {e}"));
    let file: SettingsFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {SETTINGS_PATH}: {e}"));

    let difficulty = match file.difficulty.ai.as_str() {
        "easy" => DifficultyProfile::easy(),
        "normal" => DifficultyProfile::normal(),
        "hard" => DifficultyProfile::hard(),
        other => panic!("{SETTINGS_PATH}: unknown ai difficulty '{other}' (expected easy/normal/hard)"),
    };

    GameSettings {
        difficulty,
        crit_fail_die: file.difficulty.crit_fail_die,
    }
}
