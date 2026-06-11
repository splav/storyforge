use combat_engine::ArmorId;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmorSlot {
    Chest,
    Legs,
    Feet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmorWeight {
    /// Cloth/padded. Anyone may wear light armor — no proficiency required.
    #[default]
    Light,
    /// Leather, mail. Requires the wearer's class to have the medium proficiency.
    Medium,
    /// Plate, iron. Requires the heavy proficiency.
    Heavy,
}

#[derive(Debug, Clone)]
pub struct ArmorDef {
    pub id: ArmorId,
    pub name: String,
    pub slot: ArmorSlot,
    pub weight: ArmorWeight,
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
    weight: ArmorWeight,
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

pub fn parse_armor(path: &str, src: &str, slot: ArmorSlot) -> Vec<ArmorDef> {
    let file: ArmorFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));
    file.items
        .into_iter()
        .map(|r| ArmorDef {
            id: ArmorId::from(r.id.as_str()),
            name: r.name,
            slot,
            weight: r.weight,
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

pub const CHEST_FILE: &str = "equipment/chest.toml";
pub const LEGS_FILE: &str = "equipment/legs.toml";
pub const FEET_FILE: &str = "equipment/feet.toml";

fn load_global(file: &str, slot: ArmorSlot) -> Vec<ArmorDef> {
    let path = format!("assets/data/{file}");
    if !std::path::Path::new(&path).is_file() {
        return Vec::new();
    }
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_armor(&path, &src, slot)
}

pub fn load_chest() -> Vec<ArmorDef> {
    load_global(CHEST_FILE, ArmorSlot::Chest)
}
pub fn load_legs() -> Vec<ArmorDef> {
    load_global(LEGS_FILE, ArmorSlot::Legs)
}
pub fn load_feet() -> Vec<ArmorDef> {
    load_global(FEET_FILE, ArmorSlot::Feet)
}
