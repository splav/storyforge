use serde::Deserialize;
use crate::core::StatusId;

pub const STATUS_DEFENDING: StatusId = StatusId(1);

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub id:          StatusId,
    pub name:        String,
    pub armor_bonus: i32,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StatusFile {
    statuses: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id:          u32,
    name:        String,
    armor_bonus: i32,
}

const STATUSES_PATH: &str = "assets/data/statuses.toml";

pub fn load_statuses() -> Vec<StatusDef> {
    let src = std::fs::read_to_string(STATUSES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {STATUSES_PATH}: {e}"));

    let file: StatusFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {STATUSES_PATH}: {e}"));

    file.statuses
        .into_iter()
        .map(|r| StatusDef { id: StatusId(r.id), name: r.name, armor_bonus: r.armor_bonus })
        .collect()
}
