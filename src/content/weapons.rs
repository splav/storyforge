use serde::Deserialize;
use crate::core::{DiceExpr, WeaponId};

pub const WEAPON_SHORT_SWORD: WeaponId = WeaponId(1);
pub const WEAPON_LONG_SWORD:  WeaponId = WeaponId(2);

#[derive(Debug, Clone)]
pub struct WeaponDef {
    pub id:   WeaponId,
    pub name: String,
    pub dice: DiceExpr,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WeaponFile {
    weapons: Vec<WeaponRecord>,
}

#[derive(Deserialize)]
struct WeaponRecord {
    id:         u32,
    name:       String,
    dice_count: u32,
    dice_sides: u32,
}

const WEAPONS_PATH: &str = "assets/data/weapons.toml";

pub fn load_weapons() -> Vec<WeaponDef> {
    let src = std::fs::read_to_string(WEAPONS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {WEAPONS_PATH}: {e}"));

    let file: WeaponFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {WEAPONS_PATH}: {e}"));

    file.weapons
        .into_iter()
        .map(|r| WeaponDef {
            id:   WeaponId(r.id),
            name: r.name,
            dice: DiceExpr::new(r.dice_count, r.dice_sides, 0),
        })
        .collect()
}
