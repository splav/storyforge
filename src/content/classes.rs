use serde::Deserialize;
use crate::core::{AbilityId, WeaponId};
use crate::game::components::CombatStats;

pub struct ClassDef {
    pub id:       String,
    pub name:     String,
    pub stats:    CombatStats,
    pub abilities: Vec<AbilityId>,
    pub weapon:   WeaponId,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClassFile {
    classes: Vec<ClassRecord>,
}

#[derive(Deserialize)]
struct ClassRecord {
    id:           String,
    name:         String,
    max_hp:       i32,
    armor:        i32,
    damage:       i32,
    initiative:   i32,
    intelligence: i32,
    weapon_id:    String,
    ability_ids:  Vec<String>,
}

const CLASSES_PATH: &str = "assets/data/classes.toml";

pub fn load_classes() -> Vec<ClassDef> {
    let src = std::fs::read_to_string(CLASSES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {CLASSES_PATH}: {e}"));
    let file: ClassFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {CLASSES_PATH}: {e}"));

    file.classes.into_iter().map(|r| ClassDef {
        id:       r.id,
        name:     r.name,
        stats:    CombatStats {
            max_hp:       r.max_hp,
            armor:        r.armor,
            damage:       r.damage,
            initiative:   r.initiative,
            intelligence: r.intelligence,
        },
        abilities: r.ability_ids.iter().map(|id| AbilityId::from(id.as_str())).collect(),
        weapon:    WeaponId::from(r.weapon_id.as_str()),
    }).collect()
}
