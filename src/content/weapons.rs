use serde::Deserialize;
use crate::core::{DiceExpr, WeaponId};

pub const WEAPON_SHORT_SWORD: &str = "short_sword";
pub const WEAPON_LONG_SWORD:  &str = "long_sword";
pub const WEAPON_STAFF:       &str = "staff";

#[derive(Debug, Clone)]
pub struct WeaponDef {
    pub id:          WeaponId,
    pub name:        String,
    pub dice:        DiceExpr,
    pub spell_power: i32,    // added to spell damage / healing formulas
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WeaponFile {
    weapons: Vec<WeaponRecord>,
}

#[derive(Deserialize)]
struct WeaponRecord {
    id:          String,
    name:        String,
    dice_count:  u32,
    dice_sides:  u32,
    #[serde(default)]
    spell_power: i32,
}

const WEAPONS_PATH: &str = "assets/data/weapons.toml";

pub fn load_weapons() -> Vec<WeaponDef> {
    let src = std::fs::read_to_string(WEAPONS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {WEAPONS_PATH}: {e}"));
    let file: WeaponFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {WEAPONS_PATH}: {e}"));

    file.weapons.into_iter().map(|r| WeaponDef {
        id:          WeaponId::from(r.id.as_str()),
        name:        r.name,
        dice:        DiceExpr::new(r.dice_count, r.dice_sides, 0),
        spell_power: r.spell_power,
    }).collect()
}
