use crate::core::ArmorId;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmorSlot {
    Chest,
    Legs,
    Feet,
}

#[derive(Debug, Clone)]
pub struct ArmorDef {
    pub id: ArmorId,
    pub name: String,
    pub slot: ArmorSlot,
    pub armor: i32,
    // stat bonuses
    pub max_hp: i32,
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ArmorFile {
    items: Vec<ArmorRecord>,
}

#[derive(Deserialize)]
struct ArmorRecord {
    id: String,
    name: String,
    #[serde(default)]
    armor: i32,
    #[serde(default)]
    max_hp: i32,
    #[serde(default)]
    strength: i32,
    #[serde(default)]
    dexterity: i32,
    #[serde(default)]
    constitution: i32,
    #[serde(default)]
    intelligence: i32,
    #[serde(default)]
    wisdom: i32,
    #[serde(default)]
    charisma: i32,
}

fn load_armor_file(path: &str, slot: ArmorSlot) -> Vec<ArmorDef> {
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    let file: ArmorFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));

    file.items
        .into_iter()
        .map(|r| ArmorDef {
            id: ArmorId::from(r.id.as_str()),
            name: r.name,
            slot,
            armor: r.armor,
            max_hp: r.max_hp,
            strength: r.strength,
            dexterity: r.dexterity,
            constitution: r.constitution,
            intelligence: r.intelligence,
            wisdom: r.wisdom,
            charisma: r.charisma,
        })
        .collect()
}

const CHEST_PATH: &str = "assets/data/equipment/chest.toml";
const LEGS_PATH: &str = "assets/data/equipment/legs.toml";
const FEET_PATH: &str = "assets/data/equipment/feet.toml";

pub fn load_chest() -> Vec<ArmorDef> {
    load_armor_file(CHEST_PATH, ArmorSlot::Chest)
}

pub fn load_legs() -> Vec<ArmorDef> {
    load_armor_file(LEGS_PATH, ArmorSlot::Legs)
}

pub fn load_feet() -> Vec<ArmorDef> {
    load_armor_file(FEET_PATH, ArmorSlot::Feet)
}
