use crate::core::{AbilityId, WeaponId};
use crate::game::components::CombatStats;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct EncounterDef {
    pub id: String,
    pub name: String,
    pub enemies: Vec<EnemyDef>,
}

#[derive(Debug, Clone)]
pub struct EnemyDef {
    pub name: String,
    pub stats: CombatStats,
    pub speed: i32,
    pub weapon_id: WeaponId,
    pub ability_ids: Vec<AbilityId>,
    /// Starting hex cell (col, row).
    pub hex_pos: (i32, i32),
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EncounterFile {
    encounters: Vec<EncounterRecord>,
}

#[derive(Deserialize)]
struct EncounterRecord {
    id: String,
    name: String,
    enemies: Vec<EnemyRecord>,
}

#[derive(Deserialize)]
struct EnemyRecord {
    name: String,
    max_hp: i32,
    armor: i32,
    strength: i32,
    dexterity: i32,
    constitution: i32,
    #[serde(default)]
    intelligence: i32,
    #[serde(default)]
    wisdom: i32,
    #[serde(default)]
    charisma: i32,
    speed: i32,
    weapon_id: String,
    ability_ids: Vec<String>,
    hex_col: i32,
    hex_row: i32,
}

const ENCOUNTERS_PATH: &str = "assets/data/encounters.toml";

pub fn load_encounters() -> Vec<EncounterDef> {
    let src = std::fs::read_to_string(ENCOUNTERS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ENCOUNTERS_PATH}: {e}"));
    let file: EncounterFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {ENCOUNTERS_PATH}: {e}"));

    file.encounters
        .into_iter()
        .map(|enc| EncounterDef {
            id: enc.id,
            name: enc.name,
            enemies: enc
                .enemies
                .into_iter()
                .map(|e| EnemyDef {
                    name: e.name,
                    speed: e.speed,
                    stats: CombatStats {
                        max_hp: e.max_hp,
                        armor: e.armor,
                        strength: e.strength,
                        dexterity: e.dexterity,
                        constitution: e.constitution,
                        intelligence: e.intelligence,
                        wisdom: e.wisdom,
                        charisma: e.charisma,
                    },
                    weapon_id: WeaponId::from(e.weapon_id.as_str()),
                    ability_ids: e
                        .ability_ids
                        .iter()
                        .map(|s| AbilityId::from(s.as_str()))
                        .collect(),
                    hex_pos: (e.hex_col, e.hex_row),
                })
                .collect(),
        })
        .collect()
}
