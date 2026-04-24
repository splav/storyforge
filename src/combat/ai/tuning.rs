//! AiTuning — central tuning data for AI scoring.
//! Populated incrementally across steps 2.2–2.6 (see docs/ai_rework_plan.md).
//! Currently: scaffolding only, no production code reads this yet.

use bevy::prelude::Resource;
use serde::Deserialize;

#[derive(Resource, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct AiTuning {
    pub thresholds: Thresholds,
    pub tables: Tables,
    pub difficulty: Difficulty,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Thresholds {
    // populated in step 2.2 (sanity.rs) and 2.3 (intent.rs).
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Tables {
    // populated in step 2.4 (role factor weights) and 2.5 (position eval weights).
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Difficulty {
    // populated in step 2.6 (DifficultyProfile lerp curves).
}
