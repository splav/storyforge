use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::combat::ai::config::difficulty::DifficultyProfile;

#[derive(Resource, Clone)]
pub struct GameSettings {
    pub difficulty_preset: DifficultyPreset,
    pub difficulty: DifficultyProfile,
    pub crit_fail_die: u32,
    pub ai_debug: bool,
    /// Enables JSONL decision log written to `ai_log_path` on each AI pick.
    pub ai_log: bool,
    /// Relative path for the decision log. Defaults to
    /// `logs/ai_decisions_<timestamp>.jsonl` when empty.
    pub ai_log_path: String,
    pub current_slot: u8,
    /// When true (default), the AI reuses the stored plan after a MoveOnly step
    /// instead of replanning from scratch. The fresh plan is still computed for
    /// divergence diagnostics. Set to false to restore the old replan-every-tick
    /// behaviour for comparison.
    pub ai_freeze_plan_after_move: bool,
    /// Dev-only: id of the campaign scenario to start a fresh campaign at.
    /// Empty = normal start (scenario_ids[0]). Only honoured under `--features dev`.
    pub dev_start_scenario: String,
    /// Dev-only: id of the encounter (combat scene) within the start chapter to jump
    /// to on a fresh campaign. Empty = start of chapter. Only honoured under
    /// `--features dev`. Requires `dev_start_scenario` to identify the chapter.
    pub dev_start_scene: String,
    /// Dev-only: if true, a fresh campaign starts directly in `AppState::Camp` with a
    /// seeded stash of test items instead of entering the first story scene.
    /// Only honoured under `--features dev`.
    pub dev_start_in_camp: bool,
}

impl Default for GameSettings {
    fn default() -> Self {
        let preset = DifficultyPreset::Normal;
        Self {
            difficulty_preset: preset,
            difficulty: preset.profile(),
            crit_fail_die: 20,
            ai_debug: false,
            ai_log: false,
            ai_log_path: String::new(),
            current_slot: 1,
            ai_freeze_plan_after_move: true,
            dev_start_scenario: String::new(),
            dev_start_scene: String::new(),
            dev_start_in_camp: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DifficultyPreset {
    Easy,
    Normal,
    Hard,
    Epic,
}

impl DifficultyPreset {
    pub fn profile(self) -> DifficultyProfile {
        match self {
            Self::Easy => DifficultyProfile::easy(),
            Self::Normal => DifficultyProfile::normal(),
            Self::Hard => DifficultyProfile::hard(),
            Self::Epic => DifficultyProfile::epic(),
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
        Self {
            current_slot: default_current_slot(),
        }
    }
}

fn default_current_slot() -> u8 {
    1
}

#[derive(Serialize, Deserialize, Default)]
pub struct DebugSection {
    #[serde(default)]
    pub ai_debug: bool,
    #[serde(default)]
    pub ai_log: bool,
    #[serde(default)]
    pub ai_log_path: String,
    #[serde(default = "default_true")]
    pub ai_freeze_plan_after_move: bool,
    /// Dev-only (cargo `dev` feature): id of the campaign chapter to start a NEW
    /// fresh campaign at; empty string = chapter 1 (normal behaviour).
    #[serde(default)]
    pub start_scenario: String,
    /// Dev-only (cargo `dev` feature): id of the encounter (combat scene) within the
    /// start chapter to jump to on a fresh campaign; empty = beginning of chapter.
    #[serde(default)]
    pub start_scene: String,
    /// Dev-only (cargo `dev` feature): if true, a fresh campaign starts directly in
    /// the camp screen with a seeded stash of test items.
    #[serde(default)]
    pub start_in_camp: bool,
}

fn default_true() -> bool {
    true
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
            ai_log: f.debug.ai_log,
            ai_log_path: f.debug.ai_log_path,
            current_slot: clamp_slot(f.profile.current_slot),
            ai_freeze_plan_after_move: f.debug.ai_freeze_plan_after_move,
            dev_start_scenario: f.debug.start_scenario,
            dev_start_scene: f.debug.start_scene,
            dev_start_in_camp: f.debug.start_in_camp,
        }
    }

    pub fn to_file(&self) -> SettingsFile {
        SettingsFile {
            difficulty: DifficultySection {
                ai: self.difficulty_preset,
                crit_fail_die: self.crit_fail_die,
            },
            debug: DebugSection {
                ai_debug: self.ai_debug,
                ai_log: self.ai_log,
                ai_log_path: self.ai_log_path.clone(),
                ai_freeze_plan_after_move: self.ai_freeze_plan_after_move,
                start_scenario: self.dev_start_scenario.clone(),
                start_scene: self.dev_start_scene.clone(),
                start_in_camp: self.dev_start_in_camp,
            },
            profile: ProfileSection {
                current_slot: self.current_slot,
            },
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
