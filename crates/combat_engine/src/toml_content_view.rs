//! Bevy-free `ContentView` implementation that reads `assets/data/*.toml` directly.
//!
//! Used by replay tooling (`replay_engine_trace`) and the parity test so that
//! offline tools don't need to boot a Bevy app.
//!
//! # Design note (Phase 5 D9 Path A)
//!
//! The bridge parsers in `src/content/abilities.rs` / `unit_templates.rs` import
//! `CombatStats` and `Equipment` from `crate::game::components` (Bevy-tied), so
//! they cannot be called from the engine crate.  This file duplicates the
//! TOML record structs and the mapping-to-engine-types logic.  Only the fields
//! the engine trait needs are extracted; all Bevy-specific fields (AI tags,
//! magic domains, race, faction, …) are ignored by serde.
//!
//! Engine purity (D12): this file uses only `std::fs`, `std::path`, `toml`, and
//! `serde` — no `bevy::`, no `SystemTime`, no `std::env`.

use std::{
    collections::HashMap,
    fmt,
    path::Path,
};

use serde::Deserialize;

use crate::{
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, Cost, EffectDef, StatusApplication,
        StatusBonuses, StatusDef, StatusOn, TargetType, UnitTemplate,
    },
    dice::DiceExpr,
    AbilityId, ResourceKind, StatusId,
};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur while loading content from TOML files.
#[derive(Debug)]
pub enum LoadError {
    Io { path: String, source: std::io::Error },
    Parse { path: String, message: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io { path, source } => write!(f, "IO error reading {path}: {source}"),
            LoadError::Parse { path, message } => write!(f, "Parse error in {path}: {message}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io { source, .. } => Some(source),
            LoadError::Parse { .. } => None,
        }
    }
}

// ── Public struct ─────────────────────────────────────────────────────────────

/// Bevy-free content store built by parsing `assets/data/*.toml` directly.
///
/// Implements [`ContentView`] so it can be passed to [`crate::step::step`]
/// and used by replay tooling without booting a Bevy app.
pub struct TomlContentView {
    abilities: HashMap<AbilityId, AbilityDef>,
    statuses: HashMap<StatusId, StatusDef>,
    unit_templates: HashMap<String, UnitTemplate>,
}

impl TomlContentView {
    /// Parse all content from `data_dir` (the `assets/data/` directory).
    ///
    /// Missing optional files are silently skipped (same policy as the
    /// bridge's layered loader).  Parse errors return `Err(LoadError)`.
    pub fn load_from_dir(data_dir: &Path) -> Result<Self, LoadError> {
        let abilities = load_abilities(data_dir)?;
        let statuses = load_statuses(data_dir)?;
        // Weapons and armor are needed only to compute unit_template stats.
        let weapons = load_weapons(data_dir)?;
        let armor = load_all_armor(data_dir)?;
        let unit_templates = load_unit_templates(data_dir, &weapons, &armor)?;

        Ok(Self { abilities, statuses, unit_templates })
    }

    /// Empty view — returns `None` / defaults for every query.
    /// Useful in tests that supply hand-crafted content.
    pub fn empty() -> Self {
        Self {
            abilities: HashMap::new(),
            statuses: HashMap::new(),
            unit_templates: HashMap::new(),
        }
    }
}

impl ContentView for TomlContentView {
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses {
        self.statuses.get(id).map(|d| StatusBonuses {
            speed_bonus: d.speed_bonus,
            armor_bonus: d.armor_bonus,
        }).unwrap_or_default()
    }

    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.get(id)
    }

    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        self.statuses.get(id)
    }

    fn unit_template(&self, id: &str) -> Option<UnitTemplate> {
        self.unit_templates.get(id).copied()
    }
}

// ── TOML record types (private) ───────────────────────────────────────────────
//
// These mirror the structs in `src/content/{abilities,statuses,weapons,armor,
// unit_templates}.rs` but contain only the fields the engine trait needs.
// Bevy-specific fields (AI tags, magic domains, name, race, faction, …) are
// omitted — `serde` silently ignores unknown fields by default.

// ---- abilities ---------------------------------------------------------------

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

#[derive(Deserialize)]
struct AbilityRecord {
    id: String,
    target_type: String,
    #[serde(default)]
    effect: String,
    #[serde(default = "default_range")]
    range: u32,
    #[serde(default)]
    min_range: u32,
    dice_count: Option<u32>,
    dice_sides: Option<u32>,
    #[serde(default)]
    distance: i32,
    #[serde(default)]
    costs: Vec<CostRecord>,
    #[serde(default = "default_cost_ap")]
    cost_ap: i32,
    #[serde(default)]
    aoe: String,
    #[serde(default)]
    aoe_size: u32,
    #[serde(default)]
    friendly_fire: bool,
    #[serde(default)]
    statuses: Vec<StatusApplicationRecord>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    summon_template: Option<String>,
    #[serde(default)]
    summon_max_active: Option<u32>,
}

#[derive(Deserialize)]
struct StatusApplicationRecord {
    id: String,
    on: String,
    duration: u32,
}

#[derive(Deserialize)]
struct CostRecord {
    resource: String,
    amount: i32,
}

fn default_range() -> u32 { 1 }
fn default_cost_ap() -> i32 { 1 }

// ---- statuses ----------------------------------------------------------------

#[derive(Deserialize)]
struct StatusFile {
    statuses: Vec<StatusDefRecord>,
}

#[derive(Deserialize)]
struct StatusDefRecord {
    id: String,
    #[serde(default)]
    armor_bonus: i32,
    #[serde(default)]
    damage_taken_bonus: i32,
    #[serde(default)]
    skips_turn: bool,
    #[serde(default)]
    forces_targeting: bool,
    #[serde(default)]
    blocks_mana_abilities: bool,
    #[serde(default)]
    speed_bonus: i32,
    #[serde(default)]
    hp_percent_dot: i32,
    #[serde(default)]
    causes_disadvantage: bool,
}

// ---- weapons (needed for unit_template stat computation) ---------------------

#[derive(Deserialize)]
struct WeaponFile {
    weapons: Vec<WeaponRecord>,
}

#[derive(Deserialize)]
struct WeaponRecord {
    id: String,
    #[serde(default)]
    armor: i32,
    #[serde(default)]
    max_hp: i32,
}

/// Weapon stats relevant to engine stat computation (max_hp and armor bonuses).
struct WeaponStats {
    armor: i32,
    max_hp: i32,
}

// ---- armor (chest / legs / feet) --------------------------------------------

#[derive(Deserialize)]
struct ArmorFile {
    items: Vec<ArmorRecord>,
}

#[derive(Deserialize)]
struct ArmorRecord {
    id: String,
    #[serde(default)]
    armor: i32,
    #[serde(default)]
    max_hp: i32,
}

/// Armor stats relevant to engine stat computation (max_hp and armor bonuses).
struct ArmorStats {
    armor: i32,
    max_hp: i32,
}

// ---- unit_templates ----------------------------------------------------------

#[derive(Deserialize)]
struct TemplateFile {
    #[serde(default)]
    unit_templates: Vec<TemplateRecord>,
}

#[derive(Deserialize)]
struct TemplateRecord {
    id: String,
    speed: i32,
    stats: StatsRecord,
    equipment: EquipmentRecord,
    #[serde(default)]
    resources: Option<ResourcesRecord>,
}

#[derive(Deserialize)]
struct StatsRecord {
    max_hp: i32,
}

#[derive(Deserialize)]
struct EquipmentRecord {
    main_hand: String,
    #[serde(default)]
    off_hand: Option<String>,
    chest: String,
    legs: String,
    feet: String,
}

#[derive(Deserialize, Default)]
struct ResourcesRecord {
    #[serde(default)]
    mana: i32,
    #[serde(default)]
    rage: i32,
    #[serde(default)]
    energy: i32,
}

// ── File-loading helpers ──────────────────────────────────────────────────────

fn read_toml_optional(path: &Path) -> Result<Option<String>, LoadError> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path).map(Some).map_err(|e| LoadError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

// ── Abilities ─────────────────────────────────────────────────────────────────

fn load_abilities(data_dir: &Path) -> Result<HashMap<AbilityId, AbilityDef>, LoadError> {
    let path = data_dir.join("abilities.toml");
    let src = match read_toml_optional(&path)? {
        Some(s) => s,
        None => return Ok(HashMap::new()),
    };
    let path_str = path.display().to_string();
    let file: AbilityFile = toml::from_str(&src).map_err(|e| LoadError::Parse {
        path: path_str.clone(),
        message: e.to_string(),
    })?;

    let mut map = HashMap::new();
    for r in file.abilities {
        let id = AbilityId::from(r.id.as_str());
        let def = convert_ability(r, &path_str);
        map.insert(id, def);
    }
    Ok(map)
}

fn convert_ability(r: AbilityRecord, path: &str) -> AbilityDef {
    let need_dice = |count: Option<u32>, sides: Option<u32>| {
        DiceExpr::new(
            count.unwrap_or_else(|| panic!("{path}: ability '{}' missing dice_count", r.id)),
            sides.unwrap_or_else(|| panic!("{path}: ability '{}' missing dice_sides", r.id)),
            0,
        )
    };

    let effect = match r.effect.as_str() {
        "" | "none" => EffectDef::None,
        "weapon_attack" => EffectDef::WeaponAttack,
        "damage" => EffectDef::Damage { dice: need_dice(r.dice_count, r.dice_sides) },
        "spell_damage" => EffectDef::SpellDamage { dice: need_dice(r.dice_count, r.dice_sides) },
        "heal" => EffectDef::Heal { dice: need_dice(r.dice_count, r.dice_sides) },
        "grant_movement" => EffectDef::GrantMovement { distance: r.distance },
        "restore_resources" => EffectDef::RestoreResources,
        // toggle_move_mode is UI-only; no engine effect.
        "toggle_move_mode" => EffectDef::None,
        "summon" => EffectDef::Summon {
            template_id: r.summon_template.clone().unwrap_or_else(|| {
                panic!("{path}: ability '{}' effect=summon missing summon_template", r.id)
            }),
            max_active: r.summon_max_active,
        },
        other => panic!("{path}: ability '{}' unknown effect '{other}'", r.id),
    };

    let target_type = match r.target_type.as_str() {
        "single_enemy" => TargetType::SingleEnemy,
        "single_ally" => TargetType::SingleAlly,
        "myself" => TargetType::Myself,
        "ground" => TargetType::Ground,
        other => panic!("{path}: ability '{}' unknown target_type '{other}'", r.id),
    };

    let aoe = match r.aoe.as_str() {
        "" | "none" => AoEShape::None,
        "circle" => AoEShape::Circle { radius: r.aoe_size },
        "line" => AoEShape::Line { length: r.aoe_size },
        other => panic!("{path}: ability '{}' unknown aoe '{other}'", r.id),
    };

    let costs: Vec<Cost> = r.costs.into_iter().map(|c| {
        let resource = match c.resource.as_str() {
            "hp" => ResourceKind::Hp,
            "mana" => ResourceKind::Mana,
            "rage" => ResourceKind::Rage,
            "energy" => ResourceKind::Energy,
            other => panic!("{path}: ability '{}' unknown resource '{other}'", r.id),
        };
        Cost { resource, amount: c.amount }
    }).collect();

    let statuses: Vec<StatusApplication> = r.statuses.into_iter().map(|s| {
        let on = match s.on.as_str() {
            "target" => StatusOn::Target,
            "self" => StatusOn::MySelf,
            other => panic!("{path}: ability '{}' unknown status 'on' value '{other}'", r.id),
        };
        StatusApplication {
            status: StatusId::from(s.id.as_str()),
            duration_rounds: s.duration,
            on,
        }
    }).collect();

    AbilityDef {
        key: r.key,
        cost_ap: r.cost_ap,
        costs,
        range: AbilityRange { min: r.min_range, max: r.range },
        target_type,
        aoe,
        friendly_fire: r.friendly_fire,
        effect,
        statuses,
    }
}

// ── Statuses ──────────────────────────────────────────────────────────────────

fn load_statuses(data_dir: &Path) -> Result<HashMap<StatusId, StatusDef>, LoadError> {
    let path = data_dir.join("statuses.toml");
    let src = match read_toml_optional(&path)? {
        Some(s) => s,
        None => return Ok(HashMap::new()),
    };
    let file: StatusFile = toml::from_str(&src).map_err(|e| LoadError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let mut map = HashMap::new();
    for r in file.statuses {
        let id = StatusId::from(r.id.as_str());
        let def = StatusDef {
            causes_disadvantage: r.causes_disadvantage,
            blocks_mana_abilities: r.blocks_mana_abilities,
            forces_targeting: r.forces_targeting,
            skips_turn: r.skips_turn,
            armor_bonus: r.armor_bonus,
            damage_taken_bonus: r.damage_taken_bonus,
            speed_bonus: r.speed_bonus,
            hp_percent_dot: r.hp_percent_dot,
        };
        map.insert(id, def);
    }
    Ok(map)
}

// ── Weapons ───────────────────────────────────────────────────────────────────

fn load_weapons(data_dir: &Path) -> Result<HashMap<String, WeaponStats>, LoadError> {
    let path = data_dir.join("equipment").join("weapons.toml");
    let src = match read_toml_optional(&path)? {
        Some(s) => s,
        None => return Ok(HashMap::new()),
    };
    let file: WeaponFile = toml::from_str(&src).map_err(|e| LoadError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let mut map = HashMap::new();
    for r in file.weapons {
        map.insert(r.id.clone(), WeaponStats {
            armor: r.armor,
            max_hp: r.max_hp,
        });
    }
    Ok(map)
}

// ── Armor ─────────────────────────────────────────────────────────────────────

fn load_armor_file(path: &Path) -> Result<HashMap<String, ArmorStats>, LoadError> {
    let src = match read_toml_optional(path)? {
        Some(s) => s,
        None => return Ok(HashMap::new()),
    };
    let file: ArmorFile = toml::from_str(&src).map_err(|e| LoadError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let mut map = HashMap::new();
    for r in file.items {
        map.insert(r.id.clone(), ArmorStats {
            armor: r.armor,
            max_hp: r.max_hp,
        });
    }
    Ok(map)
}

fn load_all_armor(data_dir: &Path) -> Result<HashMap<String, ArmorStats>, LoadError> {
    let eq = data_dir.join("equipment");
    let mut map = HashMap::new();
    for filename in &["chest.toml", "legs.toml", "feet.toml"] {
        let piece = load_armor_file(&eq.join(filename))?;
        map.extend(piece);
    }
    Ok(map)
}

// ── Unit templates ────────────────────────────────────────────────────────────

fn load_unit_templates(
    data_dir: &Path,
    weapons: &HashMap<String, WeaponStats>,
    armor: &HashMap<String, ArmorStats>,
) -> Result<HashMap<String, UnitTemplate>, LoadError> {
    // The bridge uses "unit_templates.toml" under campaigns/<name>/unit_templates.toml
    // but the global file lives directly under data_dir.  We only load the global layer
    // here (same as ContentView::load_global_for_tests in the bridge).
    let path = data_dir.join("unit_templates.toml");
    // unit_templates.toml may not exist at global scope; check for it.
    let src = match read_toml_optional(&path)? {
        Some(s) => s,
        None => return Ok(HashMap::new()),
    };
    let file: TemplateFile = toml::from_str(&src).map_err(|e| LoadError::Parse {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let mut map = HashMap::new();
    for r in file.unit_templates {
        let tpl = convert_template(r, weapons, armor);
        map.insert(tpl.0, tpl.1);
    }
    Ok(map)
}

/// Compute `effective_stats` (base + weapon + armor stat bonuses) and
/// `equipment_armor` inline — mirrors `ContentView::effective_stats` /
/// `ContentView::equipment_armor` in the bridge.
fn convert_template(
    r: TemplateRecord,
    weapons: &HashMap<String, WeaponStats>,
    armor_map: &HashMap<String, ArmorStats>,
) -> (String, UnitTemplate) {
    // Accumulate stat bonuses from equipment, mirroring ContentView::effective_stats
    // and ContentView::equipment_armor in the bridge.
    let mut max_hp = r.stats.max_hp;
    let mut equipment_armor = 0i32;

    // Weapons: main_hand + optional off_hand.
    for weapon_id in [Some(&r.equipment.main_hand), r.equipment.off_hand.as_ref()].into_iter().flatten() {
        if let Some(w) = weapons.get(weapon_id.as_str()) {
            max_hp += w.max_hp;
            equipment_armor += w.armor;
        }
    }
    // Armor pieces.
    for armor_id in [&r.equipment.chest, &r.equipment.legs, &r.equipment.feet] {
        if let Some(a) = armor_map.get(armor_id.as_str()) {
            max_hp += a.max_hp;
            equipment_armor += a.armor;
        }
    }

    let resources = r.resources.unwrap_or_default();

    let tpl = UnitTemplate {
        max_hp,
        armor: equipment_armor,
        base_speed: r.speed,
        max_ap: 1, // matches bridge: templates carry no max_ap; hardcoded default.
        mana_max: resources.mana,
        energy_max: resources.energy,
        rage_max: resources.rage,
    };

    (r.id, tpl)
}
