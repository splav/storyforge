use crate::core::{DiceExpr, WeaponId};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandType {
    MainHand,
    OffHand,
    TwoHanded,
}

#[derive(Debug, Clone)]
pub struct WeaponDef {
    pub id: WeaponId,
    pub name: String,
    pub hand: HandType,
    pub dice: DiceExpr,
    pub spell_power: i32, // added to spell damage / healing formulas
    // stat bonuses
    pub armor: i32,
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
struct WeaponFile {
    weapons: Vec<WeaponRecord>,
}

#[derive(Deserialize)]
struct WeaponRecord {
    id: String,
    name: String,
    #[serde(default = "default_hand")]
    hand: String,
    dice_count: u32,
    dice_sides: u32,
    #[serde(default)]
    spell_power: i32,
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

fn default_hand() -> String {
    "main_hand".to_string()
}

fn parse_hand(s: &str, weapon_id: &str) -> HandType {
    match s {
        "main_hand" => HandType::MainHand,
        "off_hand" => HandType::OffHand,
        "two_handed" => HandType::TwoHanded,
        other => panic!("weapon '{weapon_id}': unknown hand type '{other}'"),
    }
}

pub const WEAPONS_FILE: &str = "equipment/weapons.toml";

pub fn load_weapons() -> Vec<WeaponDef> {
    let path = format!("assets/data/{WEAPONS_FILE}");
    if !std::path::Path::new(&path).is_file() {
        return Vec::new();
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_weapons(&path, &src)
}

pub fn parse_weapons(path: &str, src: &str) -> Vec<WeaponDef> {
    let file: WeaponFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));
    file.weapons
        .into_iter()
        .map(|r| WeaponDef {
            hand: parse_hand(&r.hand, &r.id),
            id: WeaponId::from(r.id.as_str()),
            name: r.name,
            dice: DiceExpr::new(r.dice_count, r.dice_sides, 0),
            spell_power: r.spell_power,
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
