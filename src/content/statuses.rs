use crate::core::{DiceExpr, StatusId};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub id: StatusId,
    pub name: String,
    pub armor_bonus: i32,        // снижает урон от физических атак
    pub damage_taken_bonus: i32, // увеличивает весь получаемый урон (применяется после брони)
    pub skips_turn: bool,
    pub forces_targeting: bool,  // враги вынуждены атаковать цель с этим статусом
    pub dot_dice: Option<DiceExpr>, // кубик урона за тик (бросается один раз при наложении)
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StatusFile {
    statuses: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id: String,
    name: String,
    #[serde(default)]
    armor_bonus: i32,
    #[serde(default)]
    damage_taken_bonus: i32,
    #[serde(default)]
    skips_turn: bool,
    #[serde(default)]
    forces_targeting: bool,
    #[serde(default)]
    dot_count: Option<u32>,
    #[serde(default)]
    dot_sides: Option<u32>,
}

const STATUSES_PATH: &str = "assets/data/statuses.toml";

pub fn load_statuses() -> Vec<StatusDef> {
    let src = std::fs::read_to_string(STATUSES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {STATUSES_PATH}: {e}"));
    let file: StatusFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {STATUSES_PATH}: {e}"));

    file.statuses
        .into_iter()
        .map(|r| {
            let dot_dice = match (r.dot_count, r.dot_sides) {
                (Some(count), Some(sides)) => Some(DiceExpr::new(count, sides, 0)),
                _ => None,
            };
            StatusDef {
                id: StatusId::from(r.id.as_str()),
                name: r.name,
                armor_bonus: r.armor_bonus,
                damage_taken_bonus: r.damage_taken_bonus,
                skips_turn: r.skips_turn,
                forces_targeting: r.forces_targeting,
                dot_dice,
            }
        })
        .collect()
}
