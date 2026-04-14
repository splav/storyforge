use crate::core::{AbilityId, ArmorId, WeaponId};
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
    pub main_hand: WeaponId,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
    pub ability_ids: Vec<AbilityId>,
    pub rage_max: i32,
    pub mana_max: i32,
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
    main_hand: String,
    #[serde(default)]
    off_hand: Option<String>,
    chest: String,
    legs: String,
    feet: String,
    ability_ids: Vec<String>,
    #[serde(default)]
    rage_max: i32,
    #[serde(default)]
    mana_max: i32,
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
                        strength: e.strength,
                        dexterity: e.dexterity,
                        constitution: e.constitution,
                        intelligence: e.intelligence,
                        wisdom: e.wisdom,
                        charisma: e.charisma,
                    },
                    main_hand: WeaponId::from(e.main_hand.as_str()),
                    off_hand: e.off_hand.map(|s| WeaponId::from(s.as_str())),
                    chest: ArmorId::from(e.chest.as_str()),
                    legs: ArmorId::from(e.legs.as_str()),
                    feet: ArmorId::from(e.feet.as_str()),
                    ability_ids: e
                        .ability_ids
                        .iter()
                        .map(|s| AbilityId::from(s.as_str()))
                        .collect(),
                    rage_max: e.rage_max,
                    mana_max: e.mana_max,
                    hex_pos: (e.hex_col, e.hex_row),
                })
                .collect(),
        })
        .collect()
}
