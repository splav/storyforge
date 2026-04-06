use serde::Deserialize;
use crate::core::{AbilityId, WeaponId};
use crate::game::components::CombatStats;

#[derive(Debug, Clone)]
pub struct EncounterDef {
    pub id:      String,
    pub name:    String,
    pub enemies: Vec<EnemyDef>,
}

#[derive(Debug, Clone)]
pub struct EnemyDef {
    pub name:        String,
    pub stats:       CombatStats,
    pub weapon_id:   WeaponId,
    pub ability_ids: Vec<AbilityId>,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EncounterFile {
    encounters: Vec<EncounterRecord>,
}

#[derive(Deserialize)]
struct EncounterRecord {
    id:      String,
    name:    String,
    enemies: Vec<EnemyRecord>,
}

#[derive(Deserialize)]
struct EnemyRecord {
    name:        String,
    max_hp:      i32,
    armor:       i32,
    damage:      i32,
    initiative:  i32,
    #[serde(default)]
    intelligence: i32,
    weapon_id:   String,
    ability_ids: Vec<String>,
}

const ENCOUNTERS_PATH: &str = "assets/data/encounters.toml";

pub fn load_encounters() -> Vec<EncounterDef> {
    let src = std::fs::read_to_string(ENCOUNTERS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ENCOUNTERS_PATH}: {e}"));
    let file: EncounterFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {ENCOUNTERS_PATH}: {e}"));

    file.encounters.into_iter().map(|enc| EncounterDef {
        id:   enc.id,
        name: enc.name,
        enemies: enc.enemies.into_iter().map(|e| EnemyDef {
            name:  e.name,
            stats: CombatStats {
                max_hp:       e.max_hp,
                armor:        e.armor,
                damage:       e.damage,
                initiative:   e.initiative,
                intelligence: e.intelligence,
            },
            weapon_id:   WeaponId::from(e.weapon_id.as_str()),
            ability_ids: e.ability_ids.iter().map(|s| AbilityId::from(s.as_str())).collect(),
        }).collect(),
    }).collect()
}
