//! The effective content visible to an active scenario — merged global +
//! campaign + scenario layers with scenario winning on id clash, then campaign,
//! then global.
//!
//! This is the single source of rule lookups for combat systems. The previous
//! `GameDb` fields for abilities/statuses/weapons/armor/classes/races/etc. are
//! gone; they now live here and are exposed as the `ActiveContent` resource
//! populated on scenario entry.

use crate::content::abilities::{parse_abilities, AbilityDef, ABILITIES_FILE};
use crate::content::armor::{parse_armor, ArmorDef, ArmorSlot, CHEST_FILE, FEET_FILE, LEGS_FILE};
use crate::content::classes::{parse_classes, ClassDef, CLASSES_FILE};
use crate::content::races::{parse_races, FactionDef, PathDef, RaceDef, RACES_FILE};
use crate::content::statuses::{parse_statuses, StatusDef, STATUSES_FILE};
use crate::content::unit_templates::{
    parse_unit_templates, UnitTemplateDef, UNIT_TEMPLATES_FILE,
};
use crate::combat::ai::tuning::AiTuning;
use crate::content::weapons::{parse_weapons, WeaponDef, WEAPONS_FILE};
use crate::core::{AbilityId, ArmorId, StatusId, WeaponId};
use crate::game::components::{CombatStats, Equipment};
use bevy::prelude::*;
use std::collections::HashMap;
use std::path::Path;

/// Complete rules set for a scenario (or a test harness).
#[derive(Default, Clone, Debug)]
pub struct ContentView {
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

impl ContentView {
    /// Effective CombatStats = base + sum of all equipped weapon/armor stat bonuses.
    pub fn effective_stats(&self, base: &CombatStats, equipment: &Equipment) -> CombatStats {
        let mut s = base.clone();
        for weapon_id in [&equipment.main_hand, &equipment.off_hand].into_iter().flatten() {
            if let Some(w) = self.weapons.get(weapon_id) {
                s.max_hp += w.max_hp;
                s.strength += w.strength;
                s.dexterity += w.dexterity;
                s.constitution += w.constitution;
                s.intelligence += w.intelligence;
                s.wisdom += w.wisdom;
                s.charisma += w.charisma;
            }
        }
        for armor_id in [&equipment.chest, &equipment.legs, &equipment.feet] {
            if let Some(a) = self.armor.get(armor_id) {
                s.max_hp += a.max_hp;
                s.strength += a.strength;
                s.dexterity += a.dexterity;
                s.constitution += a.constitution;
                s.intelligence += a.intelligence;
                s.wisdom += a.wisdom;
                s.charisma += a.charisma;
            }
        }
        s
    }

    /// Total armor from all equipment pieces (armor items + weapons like shields).
    pub fn equipment_armor(&self, equipment: &Equipment) -> i32 {
        let mut total = 0;
        for weapon_id in [&equipment.main_hand, &equipment.off_hand].into_iter().flatten() {
            if let Some(w) = self.weapons.get(weapon_id) {
                total += w.armor;
            }
        }
        for armor_id in [&equipment.chest, &equipment.legs, &equipment.feet] {
            if let Some(a) = self.armor.get(armor_id) {
                total += a.armor;
            }
        }
        total
    }
}

impl ContentView {
    /// Loads + merges content from global / campaign / scenario layers. Files at
    /// each layer are optional — missing files just contribute nothing. IDs are
    /// overridden wholesale: scenario beats campaign beats global.
    pub fn load_layered(campaign_dir: &Path, scenario_dir: &Path) -> Self {
        let global = Path::new("assets/data");
        let layers = [global, campaign_dir, scenario_dir];

        let mut v = ContentView::default();

        // Each content type: read the file at every layer (if present), merge by id.
        for base in layers {
            merge_into_map(base, ABILITIES_FILE, parse_abilities, |a| a.id.clone(), &mut v.abilities);
            merge_into_map(base, STATUSES_FILE, parse_statuses, |s| s.id.clone(), &mut v.statuses);
            merge_into_map(base, WEAPONS_FILE, parse_weapons, |w| w.id.clone(), &mut v.weapons);
            merge_into_map(base, CLASSES_FILE, parse_classes, |c| c.id.clone(), &mut v.classes);
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

fn merge_armor(
    base: &Path,
    file: &str,
    slot: ArmorSlot,
    dst: &mut HashMap<ArmorId, ArmorDef>,
) {
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
    for r in rs { races.insert(r.id.clone(), r); }
    for f in fs { factions.insert(f.id.clone(), f); }
    for p in ps { paths.insert(p.id.clone(), p); }
}

impl ContentView {
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

/// Currently-active content view. Set to the current scenario's merged view
/// on scenario entry; defaults to empty outside combat.
#[derive(Resource, Default)]
pub struct ActiveContent(pub ContentView);

impl std::ops::Deref for ActiveContent {
    type Target = ContentView;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
