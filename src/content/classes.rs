use crate::content::armor::ArmorWeight;
use crate::game::components::CombatStats;
use combat_engine::{AbilityId, ArmorId, WeaponId};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub id: String,
    pub name: String,
    pub stats: CombatStats,
    pub speed: i32,
    pub abilities: Vec<AbilityId>,
    pub main_hand: WeaponId,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
    pub rage_max: i32,   // 0 — нет механики ярости
    pub mana_max: i32,   // 0 — нет механики маны
    pub energy_max: i32, // 0 — нет механики энергии
    /// Medium/Heavy armor weights this class is trained in. Light armor is always
    /// allowed and never listed. Empty = light-only (e.g. mage/healer).
    /// Camp-screen gate only — not enforced in combat.
    pub armor_proficiencies: Vec<ArmorWeight>,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClassFile {
    classes: Vec<ClassRecord>,
}

#[derive(Deserialize)]
struct ClassRecord {
    id: String,
    name: String,
    max_hp: i32,
    strength: i32,
    dexterity: i32,
    constitution: i32,
    intelligence: i32,
    wisdom: i32,
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
    #[serde(default)]
    energy_max: i32,
    #[serde(default)]
    armor_proficiencies: Vec<ArmorWeight>,
}

pub const CLASSES_FILE: &str = "classes.toml";

pub fn load_classes() -> Vec<ClassDef> {
    let path = format!("assets/data/{CLASSES_FILE}");
    if !std::path::Path::new(&path).is_file() {
        return Vec::new();
    }
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_classes(&path, &src)
}

pub fn parse_classes(path: &str, src: &str) -> Vec<ClassDef> {
    let file: ClassFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));
    file.classes
        .into_iter()
        .map(|r| ClassDef {
            id: r.id,
            name: r.name,
            speed: r.speed,
            stats: CombatStats {
                max_hp: r.max_hp,
                strength: r.strength,
                dexterity: r.dexterity,
                constitution: r.constitution,
                intelligence: r.intelligence,
                wisdom: r.wisdom,
                charisma: r.charisma,
            },
            abilities: r
                .ability_ids
                .iter()
                .map(|id| AbilityId::from(id.as_str()))
                .collect(),
            main_hand: WeaponId::from(r.main_hand.as_str()),
            off_hand: r.off_hand.map(|s| WeaponId::from(s.as_str())),
            chest: ArmorId::from(r.chest.as_str()),
            legs: ArmorId::from(r.legs.as_str()),
            feet: ArmorId::from(r.feet.as_str()),
            rage_max: r.rage_max,
            mana_max: r.mana_max,
            energy_max: r.energy_max,
            armor_proficiencies: r.armor_proficiencies,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_classes_have_correct_armor_proficiencies() {
        let src = include_str!("../../assets/data/classes.toml");
        let classes = parse_classes("assets/data/classes.toml", src);

        let find = |id: &str| {
            classes
                .iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("class '{id}' not found"))
                .armor_proficiencies
                .clone()
        };

        assert_eq!(
            find("warrior"),
            vec![ArmorWeight::Medium, ArmorWeight::Heavy]
        );
        assert_eq!(find("ranger"), vec![ArmorWeight::Medium]);
        assert_eq!(find("mage"), Vec::<ArmorWeight>::new());
    }
}
