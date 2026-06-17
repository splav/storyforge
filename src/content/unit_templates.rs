//! Global reusable unit stat blocks (data-only).
//!
//! Anything instantiating a combatant (encounter enemies, boss phases, future
//! summons) can reference a template by id and override individual scalar fields
//! or replace a whole `stats` / `equipment` / `resources` block.
//!
//! Templates have no hex position — supplied at the use site.

use crate::combat::ai::config::tuning::AiTuningOverride;
use crate::game::components::CombatStats;
use combat_engine::{AbilityId, ArmorId, WeaponId};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct UnitTemplateDef {
    pub id: String,
    pub name: String,
    pub race: String,
    pub faction: Option<String>,
    pub path: Option<String>,
    pub speed: i32,
    pub stats: CombatStats,
    pub equipment: EquipmentBlock,
    pub resources: ResourcesBlock,
    pub ability_ids: Vec<AbilityId>,
    /// Per-unit AiTuning override. `None` for all current units.
    /// Populated from `ai_tuning_override` in `unit_templates.toml`.
    pub ai_tuning_override: Option<AiTuningOverride>,
    /// Statuses applied at combat bootstrap with `PERMANENT_DURATION`.
    /// Used for non-acting party NPCs that must skip every turn
    /// (e.g. `stunned` on `wounded_magister`).
    pub initial_statuses: Vec<String>,
    /// Optional starting pool overrides (per-kind). `None` for a kind →
    /// default policy (Hp/Mana/Energy/Ap/Mp = max; Rage = 0).
    /// Populated from TOML `initial_pools = { hp = 6 }`.
    pub initial_pools: std::collections::HashMap<String, i32>,
}

#[derive(Debug, Clone)]
pub struct EquipmentBlock {
    pub main_hand: WeaponId,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
}

#[derive(Debug, Clone, Default)]
pub struct ResourcesBlock {
    pub rage_max: i32,
    pub mana_max: i32,
    pub energy_max: i32,
}

// ── TOML records (nested blocks) ─────────────────────────────────────────────

#[derive(Deserialize, Clone)]
pub struct TemplateRecord {
    pub id: String,
    pub name: String,
    pub race: String,
    #[serde(default)]
    pub faction: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub speed: i32,
    pub stats: StatsRecord,
    pub equipment: EquipmentRecord,
    #[serde(default)]
    pub resources: Option<ResourcesRecord>,
    pub ability_ids: Vec<String>,
    #[serde(default)]
    pub ai_tuning_override: Option<AiTuningOverride>,
    #[serde(default)]
    pub initial_statuses: Vec<String>,
    /// Optional starting pool values. TOML format: `initial_pools = { hp = 6 }`.
    /// Keys are lowercase pool kind names (hp, mana, rage, energy, ap, mp).
    /// Absent keys → default per-kind policy (see `template_starting_pool`).
    #[serde(default)]
    pub initial_pools: std::collections::HashMap<String, i32>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct StatsRecord {
    pub max_hp: i32,
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    #[serde(default)]
    pub intelligence: i32,
    #[serde(default)]
    pub wisdom: i32,
    #[serde(default)]
    pub charisma: i32,
}

impl From<StatsRecord> for CombatStats {
    fn from(r: StatsRecord) -> Self {
        CombatStats {
            max_hp: r.max_hp,
            strength: r.strength,
            dexterity: r.dexterity,
            constitution: r.constitution,
            intelligence: r.intelligence,
            wisdom: r.wisdom,
            charisma: r.charisma,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct EquipmentRecord {
    pub main_hand: String,
    #[serde(default)]
    pub off_hand: Option<String>,
    pub chest: String,
    pub legs: String,
    pub feet: String,
}

impl From<EquipmentRecord> for EquipmentBlock {
    fn from(r: EquipmentRecord) -> Self {
        EquipmentBlock {
            main_hand: WeaponId::from(r.main_hand.as_str()),
            off_hand: r.off_hand.map(|s| WeaponId::from(s.as_str())),
            chest: ArmorId::from(r.chest.as_str()),
            legs: ArmorId::from(r.legs.as_str()),
            feet: ArmorId::from(r.feet.as_str()),
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ResourcesRecord {
    #[serde(default)]
    pub mana: i32,
    #[serde(default)]
    pub rage: i32,
    #[serde(default)]
    pub energy: i32,
}

impl From<ResourcesRecord> for ResourcesBlock {
    fn from(r: ResourcesRecord) -> Self {
        ResourcesBlock {
            mana_max: r.mana,
            rage_max: r.rage,
            energy_max: r.energy,
        }
    }
}

// ── Conversion ──────────────────────────────────────────────────────────────

pub const UNIT_TEMPLATES_FILE: &str = "unit_templates.toml";

#[derive(Deserialize)]
struct TemplateFile {
    #[serde(default)]
    unit_templates: Vec<TemplateRecord>,
}

pub fn parse_unit_templates(path: &str, src: &str) -> Vec<UnitTemplateDef> {
    let file: TemplateFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));
    file.unit_templates
        .into_iter()
        .map(convert_template_record)
        .collect()
}

/// Converts a raw TOML record into the runtime template. Reused by the campaign
/// loader for `<campaign>/unit_templates.toml` and by scenarios for `characters.toml`.
pub fn convert_template_record(r: TemplateRecord) -> UnitTemplateDef {
    UnitTemplateDef {
        id: r.id,
        name: r.name,
        race: r.race,
        faction: r.faction,
        path: r.path,
        speed: r.speed,
        stats: r.stats.into(),
        equipment: r.equipment.into(),
        resources: r.resources.map(Into::into).unwrap_or_default(),
        ability_ids: r
            .ability_ids
            .into_iter()
            .map(|s| AbilityId::from(s.as_str()))
            .collect(),
        ai_tuning_override: r.ai_tuning_override,
        initial_statuses: r.initial_statuses,
        initial_pools: r.initial_pools,
    }
}
