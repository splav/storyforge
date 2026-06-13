//! The effective content visible to an active scenario — merged global +
//! campaign + scenario layers with scenario winning on id clash, then campaign,
//! then global.
//!
//! This is the single source of rule lookups for combat systems. The previous
//! `GameDb` fields for abilities/statuses/weapons/armor/classes/races/etc. are
//! gone; they now live here and are exposed as the `ActiveContent` resource
//! populated on scenario entry.

use crate::combat::ai::config::tuning::AiTuning;
use crate::content::abilities::{parse_abilities, AbilityDef, ABILITIES_FILE};
use crate::content::armor::{parse_armor, ArmorDef, ArmorSlot, CHEST_FILE, FEET_FILE, LEGS_FILE};
use crate::content::classes::{parse_classes, ClassDef, CLASSES_FILE};
use crate::content::races::{parse_races, FactionDef, PathDef, RaceDef, RACES_FILE};
use crate::content::statuses::{parse_statuses, StatusDef, STATUSES_FILE};
use crate::content::unit_templates::{parse_unit_templates, UnitTemplateDef, UNIT_TEMPLATES_FILE};
use crate::content::weapons::{parse_weapons, WeaponDef, WEAPONS_FILE};
use crate::game::components::{CombatStats, Equipment};
use bevy::prelude::*;
use combat_engine::{AbilityId, ArmorId, StatusId, WeaponId};
use std::collections::HashMap;
use std::path::Path;

/// Complete rules set for a scenario (or a test harness).
#[derive(Default, Clone, Debug)]
pub struct ActiveContentData {
    pub abilities: HashMap<AbilityId, AbilityDef>,
    /// Abilities that declare a custom hotkey (`key = "..."`) — in load order.
    /// Universal: every combatant may use them.
    pub keyed_abilities: Vec<AbilityId>,
    pub statuses: HashMap<StatusId, StatusDef>,
    pub weapons: HashMap<WeaponId, WeaponDef>,
    pub armor: HashMap<ArmorId, ArmorDef>,
    pub classes: HashMap<String, ClassDef>,
    pub unit_templates: HashMap<String, UnitTemplateDef>,
    pub races: HashMap<String, RaceDef>,
    pub factions: HashMap<String, FactionDef>,
    pub paths: HashMap<String, PathDef>,
    pub ai_tuning: AiTuning,
}

impl ActiveContentData {
    /// Effective CombatStats = base + sum of all equipped weapon/armor stat bonuses.
    pub fn effective_stats(&self, base: &CombatStats, equipment: &Equipment) -> CombatStats {
        let mut s = base.clone();
        for weapon_id in [&equipment.main_hand, &equipment.off_hand]
            .into_iter()
            .flatten()
        {
            if let Some(w) = self.weapons.get(weapon_id) {
                s += &w.stats.combat;
            }
        }
        for armor_id in [&equipment.chest, &equipment.legs, &equipment.feet] {
            if let Some(a) = self.armor.get(armor_id) {
                s += &a.stats.combat;
            }
        }
        s
    }

    /// Total armor from all equipment pieces (armor items + weapons like shields).
    pub fn equipment_armor(&self, equipment: &Equipment) -> i32 {
        let mut total = 0;
        for weapon_id in [&equipment.main_hand, &equipment.off_hand]
            .into_iter()
            .flatten()
        {
            if let Some(w) = self.weapons.get(weapon_id) {
                total += w.stats.armor;
            }
        }
        for armor_id in [&equipment.chest, &equipment.legs, &equipment.feet] {
            if let Some(a) = self.armor.get(armor_id) {
                total += a.stats.armor;
            }
        }
        total
    }

    /// Total magic_resist from all equipment pieces (armor items + weapons like shields).
    ///
    /// Mirrors `equipment_armor` — iterates main/off-hand weapons and
    /// chest/legs/feet armor slots, summing `stats.magic_resist`.
    pub fn equipment_magic_resist(&self, equipment: &Equipment) -> i32 {
        let mut total = 0;
        for weapon_id in [&equipment.main_hand, &equipment.off_hand]
            .into_iter()
            .flatten()
        {
            if let Some(w) = self.weapons.get(weapon_id) {
                total += w.stats.magic_resist;
            }
        }
        for armor_id in [&equipment.chest, &equipment.legs, &equipment.feet] {
            if let Some(a) = self.armor.get(armor_id) {
                total += a.stats.magic_resist;
            }
        }
        total
    }

    /// Flat mana-pool bonus from worn armor. Sums the three armor slots' `mana`.
    /// Weapons are intentionally excluded (no weapon carries mana today); the
    /// armor-only shape makes that exclusion explicit — adding weapon-mana later
    /// is a deliberate change, not a silent flip.
    pub fn equipment_mana_bonus(&self, equipment: &Equipment) -> i32 {
        [&equipment.chest, &equipment.legs, &equipment.feet]
            .into_iter()
            .filter_map(|id| self.armor.get(id))
            .map(|a| a.stats.mana)
            .sum()
    }
}

impl ActiveContentData {
    /// Loads + merges content from global / campaign / scenario layers. Files at
    /// each layer are optional — missing files just contribute nothing. IDs are
    /// overridden wholesale: scenario beats campaign beats global.
    pub fn load_layered(campaign_dir: &Path, scenario_dir: &Path) -> Self {
        let global = Path::new("assets/data");
        let layers = [global, campaign_dir, scenario_dir];

        let mut v = ActiveContentData::default();

        // Each content type: read the file at every layer (if present), merge by id.
        for base in layers {
            merge_into_map(
                base,
                ABILITIES_FILE,
                parse_abilities,
                |a| a.id.clone(),
                &mut v.abilities,
            );
            merge_into_map(
                base,
                STATUSES_FILE,
                parse_statuses,
                |s| s.id.clone(),
                &mut v.statuses,
            );
            merge_into_map(
                base,
                WEAPONS_FILE,
                parse_weapons,
                |w| w.id.clone(),
                &mut v.weapons,
            );
            merge_into_map(
                base,
                CLASSES_FILE,
                parse_classes,
                |c| c.id.clone(),
                &mut v.classes,
            );
            merge_into_map(
                base,
                UNIT_TEMPLATES_FILE,
                parse_unit_templates,
                |t| t.id.clone(),
                &mut v.unit_templates,
            );

            // Armor is a triple of files — same slot parser, different slots.
            merge_armor(base, CHEST_FILE, ArmorSlot::Chest, &mut v.armor);
            merge_armor(base, LEGS_FILE, ArmorSlot::Legs, &mut v.armor);
            merge_armor(base, FEET_FILE, ArmorSlot::Feet, &mut v.armor);

            // Races file is a 3-in-1: races + factions + paths.
            merge_races(base, &mut v.races, &mut v.factions, &mut v.paths);
        }

        // AiTuning: singleton config, loaded with last-layer-wins override.
        // Currently only the global layer carries content (all three layers produce
        // default() because the TOML is empty). Layered field-level merging will be
        // added alongside step 2.2+ when fields are actually populated.
        for base in layers {
            let path = base.join("ai_tuning.toml");
            if path.is_file() {
                let src = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
                v.ai_tuning = toml::from_str(&src)
                    .unwrap_or_else(|e| panic!("Cannot parse {}: {e}", path.display()));
            }
        }

        // Derived: keyed_abilities — abilities that declare a hotkey, in load order.
        // `merge_into_map` uses HashMap (no order) so we sort by id for determinism.
        let mut keyed: Vec<AbilityId> = v
            .abilities
            .values()
            .filter(|a| a.key.is_some())
            .map(|a| a.id.clone())
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        v.keyed_abilities = keyed;

        v
    }
}

fn merge_into_map<T, K: std::hash::Hash + Eq, P, F>(
    base: &Path,
    file: &str,
    parse: P,
    key_of: F,
    dst: &mut HashMap<K, T>,
) where
    P: Fn(&str, &str) -> Vec<T>,
    F: Fn(&T) -> K,
{
    let path = base.join(file);
    if !path.is_file() {
        return;
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    for item in parse(&path.display().to_string(), &src) {
        dst.insert(key_of(&item), item);
    }
}

fn merge_armor(base: &Path, file: &str, slot: ArmorSlot, dst: &mut HashMap<ArmorId, ArmorDef>) {
    let path = base.join(file);
    if !path.is_file() {
        return;
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    for item in parse_armor(&path.display().to_string(), &src, slot) {
        dst.insert(item.id.clone(), item);
    }
}

fn merge_races(
    base: &Path,
    races: &mut HashMap<String, RaceDef>,
    factions: &mut HashMap<String, FactionDef>,
    paths: &mut HashMap<String, PathDef>,
) {
    let path = base.join(RACES_FILE);
    if !path.is_file() {
        return;
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    let (rs, fs, ps) = parse_races(&path.display().to_string(), &src);
    for r in rs {
        races.insert(r.id.clone(), r);
    }
    for f in fs {
        factions.insert(f.id.clone(), f);
    }
    for p in ps {
        paths.insert(p.id.clone(), p);
    }
}

impl ActiveContentData {
    /// Test-only: loads the fully-merged view from the global layer ONLY
    /// (`assets/data/*.toml`), without any campaign/scenario overrides.
    /// Useful for unit tests that don't care about scenario-specific overrides.
    #[cfg(any(test, debug_assertions))]
    pub fn load_global_for_tests() -> Self {
        let global = std::path::Path::new("assets/data");
        // Walk every layered merge function with the global path twice (no overrides).
        // We reuse load_layered by treating "no campaign/scenario" as same path.
        Self::load_layered(global, global)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::armor::{ArmorDef, ArmorSlot, ArmorWeight};
    use crate::content::item_stats::ItemStats;
    use crate::content::weapons::WeaponDef;
    use crate::game::components::Equipment;
    use combat_engine::{ArmorId, DiceExpr, WeaponId};

    fn armor_with_magic_resist(id: &str, mr: i32) -> (ArmorId, ArmorDef) {
        let aid = ArmorId::from(id);
        let def = ArmorDef {
            id: aid.clone(),
            name: id.to_string(),
            slot: ArmorSlot::Chest,
            weight: ArmorWeight::Light,
            image: None,
            stats: ItemStats {
                magic_resist: mr,
                ..Default::default()
            },
        };
        (aid, def)
    }

    fn weapon_with_magic_resist(id: &str, mr: i32) -> (WeaponId, WeaponDef) {
        use crate::content::weapons::HandType;
        let wid = WeaponId::from(id);
        let def = WeaponDef {
            id: wid.clone(),
            name: id.to_string(),
            hand: HandType::MainHand,
            dice: DiceExpr::new(1, 6, 0),
            ranged: false,
            spell_power: 0,
            image: None,
            stats: ItemStats {
                magic_resist: mr,
                ..Default::default()
            },
        };
        (wid, def)
    }

    /// equipment_magic_resist sums magic_resist from all equipped armor + weapon slots.
    #[test]
    fn equipment_magic_resist_sums_across_slots() {
        let (chest_id, chest_def) = armor_with_magic_resist("robe", 2);
        let (weapon_id, weapon_def) = weapon_with_magic_resist("focus", 1);

        let mut content = ActiveContentData::default();
        content.armor.insert(chest_id.clone(), chest_def);
        content.weapons.insert(weapon_id.clone(), weapon_def);

        let equipment = Equipment {
            main_hand: Some(weapon_id),
            off_hand: None,
            chest: chest_id,
            legs: ArmorId::from(""),
            feet: ArmorId::from(""),
        };

        // chest contributes 2, weapon contributes 1; unknown legs/feet → 0.
        assert_eq!(content.equipment_magic_resist(&equipment), 3);
    }

    /// equipment_magic_resist returns 0 when no item has magic_resist.
    #[test]
    fn equipment_magic_resist_zero_when_no_item_has_it() {
        let (chest_id, chest_def) = armor_with_magic_resist("plain_robe", 0);
        let mut content = ActiveContentData::default();
        content.armor.insert(chest_id.clone(), chest_def);

        let equipment = Equipment {
            main_hand: None,
            off_hand: None,
            chest: chest_id,
            legs: ArmorId::from(""),
            feet: ArmorId::from(""),
        };

        assert_eq!(content.equipment_magic_resist(&equipment), 0);
    }
}

/// Currently-active content view. Set to the current scenario's merged view
/// on scenario entry; defaults to empty outside combat.
#[derive(Resource, Default)]
pub struct ActiveContent(pub ActiveContentData);

impl std::ops::Deref for ActiveContent {
    type Target = ActiveContentData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
