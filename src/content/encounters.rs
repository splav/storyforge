use crate::content::unit_templates::{
    EquipmentBlock, EquipmentRecord, ResourcesBlock, ResourcesRecord, StatsRecord, UnitTemplateDef,
};
use crate::game::components::CombatStats;
use combat_engine::{AbilityId, ArmorId, WeaponId};
use serde::Deserialize;
use std::collections::HashMap;

/// A resolved environmental hazard object placed on the grid. `hex`/`ability`/
/// `owner` are content-facing; engine fields (`id`, `revealed_to`) are filled at
/// bootstrap.
#[derive(Debug, Clone)]
pub struct EnvObjectDef {
    pub hex: hexx::Hex,
    pub ability: AbilityId,
    /// Which team owns (placed) this trap. `None` = neutral hazard visible only
    /// after discovery by either team.
    pub owner: Option<combat_engine::state::Team>,
}

#[derive(Debug, Clone)]
pub struct EncounterDef {
    pub id: String,
    pub name: String,
    pub enemies: Vec<EnemyDef>,
    pub victory: VictoryCondition,
    /// Static obstacle hexes — blocks movement and LOS. Populated into
    /// `CombatState.blocked_hexes` on bootstrap.
    pub obstacles: Vec<hexx::Hex>,
    /// Environmental hazard objects (traps, etc.) placed on the grid.
    /// Populated into `CombatState.environment` on bootstrap.
    pub environment: Vec<EnvObjectDef>,
    /// What happens on defeat (default `Retry`).
    pub on_defeat: OnDefeat,
    /// Secondary objectives evaluated at combat end. Default empty.
    pub objectives: Vec<ObjectiveDef>,
    /// Bare item ids granted to the party stash on victory. Default empty.
    /// Each id is resolved at grant time: weapon → `ItemRef::Weapon`,
    /// armor → `ItemRef::Armor`. Unknown ids are skipped with a warning.
    pub rewards: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub enum VictoryCondition {
    /// Default — combat ends when no enemy is alive.
    #[default]
    AllEnemiesDead,
    /// Combat ends the moment a specific enemy dies (other enemies may live).
    /// `enemy_name` must match `EnemyDef.name` exactly.
    KillTarget {
        enemy_name: String,
        marker_color: [f32; 3],
        description: Option<String>,
    },
    /// Combat fails immediately if the named unit (matched by `Name`) is dead.
    /// Leaf condition — succeeds only when paired inside `AllOf`; alone it
    /// only produces defeat signals, never a standalone victory.
    KeepAlive {
        target_name: String,
        marker_color: [f32; 3],
    },
    /// Conjunction — all sub-conditions must hold. Short-circuits on first
    /// defeat. Victory when every sub-condition resolves to `Some(true)`.
    AllOf(Vec<VictoryCondition>),
}

impl VictoryCondition {
    /// Short Russian description shown in the combat HUD as the player's goal.
    pub fn objective_text(&self) -> String {
        match self {
            VictoryCondition::AllEnemiesDead => "Победить всех врагов".into(),
            VictoryCondition::KillTarget {
                description: Some(d),
                ..
            } => d.clone(),
            VictoryCondition::KillTarget { enemy_name, .. } => format!("убить {enemy_name}"),
            VictoryCondition::KeepAlive { target_name, .. } => {
                format!("сохранить жизнь {target_name}")
            }
            VictoryCondition::AllOf(conditions) => conditions
                .iter()
                .map(|c| c.objective_text())
                .collect::<Vec<_>>()
                .join(" и "),
        }
    }
}

/// Default red-ish ring color when `marker_color` is not specified in TOML.
pub const DEFAULT_TARGET_MARKER: [f32; 3] = [0.90, 0.15, 0.15];

/// What happens when the player loses this encounter.
/// `Retry` (default) = current behavior (restart combat).
/// `Proceed` = the scenario advances anyway (lose-but-proceed bout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDefeat {
    #[default]
    Retry,
    Proceed,
}

/// A secondary objective evaluated at combat end (on victory OR proceed-defeat).
/// When `condition` holds in the final state, the flag `id` is recorded into
/// campaign flags. `hidden` suppresses HUD display (revealed only by outcome).
#[derive(Debug, Clone)]
pub struct ObjectiveDef {
    pub id: String,
    pub condition: VictoryCondition,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct EnemyDef {
    pub name: String,
    pub race: String,
    pub faction: Option<String>,
    pub path: Option<String>,
    /// Asset path relative to `assets/images/` for the battle figurine sprite.
    /// Literal (no `{race}` substitution). Inherits the template's `sprite`
    /// when absent. `None` → colored-circle fallback.
    pub sprite: Option<String>,
    pub stats: CombatStats,
    pub speed: i32,
    pub main_hand: WeaponId,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
    pub ability_ids: Vec<AbilityId>,
    pub rage_max: i32,
    pub mana_max: i32,
    pub energy_max: i32,
    /// Starting hex cell.
    pub hex_pos: hexx::Hex,
    /// Phase transitions in declaration order; each fires at most once.
    pub phases: Vec<PhaseDef>,
    /// Optional passive aura: while this enemy is alive, every unit matching
    /// `affects` within `radius` hexes gets `status` reapplied each TurnStart.
    pub aura: Option<AuraDef>,
    /// Creature tags (e.g. `"undead"`, `"beast"`, `"living"`).
    /// Populated into `Tags` ECS component at spawn; empty for most enemies.
    pub tags: std::collections::BTreeSet<combat_engine::TagId>,
}

#[derive(Debug, Clone)]
pub struct AuraDef {
    pub status: combat_engine::StatusId,
    pub radius: u32,
    pub affects: AuraAffects,
    /// Tags the target must carry for this aura to apply.  Empty ⇒ no filter.
    pub affects_tags: std::collections::BTreeSet<combat_engine::TagId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuraAffects {
    /// Applies only to the opposite team.
    Enemies,
    /// Applies only to same-team units (excluding the source itself).
    Allies,
    /// Applies to everyone in range except the source itself.
    All,
}

/// One-step transformation applied to an enemy when its trigger fires.
/// Missing fields keep their current value.
#[derive(Debug, Clone)]
pub struct PhaseDef {
    pub trigger: PhaseTrigger,
    pub name: Option<String>,
    pub stats: Option<CombatStats>,
    pub ability_ids: Option<Vec<AbilityId>>,
    pub heal_to_full: bool,
    /// Narrative blurb shown in the transition popup/log.
    pub flavor: Option<String>,
    /// When set, replaces the active `CombatObjective` the moment this phase fires.
    pub victory_override: Option<VictoryCondition>,
    /// Number of rounds (from phase activation) the player has to fulfil the
    /// new objective. Expires → defeat (boss escaped / time ran out).
    pub turn_limit: Option<u32>,
    /// When set, overrides the unit's AI evaluation regime each turn.
    pub ai_behavior: Option<AiBehaviorKind>,
    /// `Some` = REPLACE the unit's tags on phase entry; `None` = keep current tags.
    pub tags: Option<std::collections::BTreeSet<combat_engine::TagId>>,
    /// Equipment block from this phase's template (split into slots at build
    /// time). `None` = keep current armor/magic_resist.
    pub equipment: Option<EquipmentBlock>,
    /// Base movement speed from this phase's template.
    /// `None` = keep current base_speed.
    pub base_speed: Option<i32>,
}

/// AI evaluation-regime override applied when a boss phase fires.
/// A unit with this override evaluates each turn under the specified regime
/// instead of the normal tactical pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiBehaviorKind {
    /// Unit maximises distance from the nearest enemy each turn.
    /// Offensive casts are suppressed; self-heal/self-buff are allowed.
    Flee,
}

#[derive(Debug, Clone)]
pub enum PhaseTrigger {
    /// Fires when `hp * 100 <= max_hp * pct` (once).
    HpBelowPct(i32),
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EncounterFile {
    encounters: Vec<EncounterRecord>,
}

#[derive(Deserialize)]
struct EncounterRecord {
    id: String,
    name: String,
    enemies: Vec<EnemyRecord>,
    #[serde(default)]
    victory: Option<VictoryRecord>,
    #[serde(default)]
    obstacles: Vec<ObstacleRecord>,
    #[serde(default)]
    environment: Vec<EnvRecord>,
    #[serde(default)]
    on_defeat: Option<String>,
    #[serde(default)]
    objectives: Vec<ObjectiveRecord>,
    #[serde(default)]
    rewards: Vec<String>,
}

/// A static obstacle tile as declared in `encounters.toml`.
/// Blocked for both movement pass-through and stopping.
#[derive(Deserialize)]
struct ObstacleRecord {
    hex_col: i32,
    hex_row: i32,
}

/// A hidden environmental hazard as declared in `encounters.toml`.
///
/// TOML syntax: `[[encounters.environment]]` with fields `hex_col`, `hex_row`,
/// `ability`, and optional `owner` (`"player"`, `"enemy"`, or absent for neutral).
#[derive(Deserialize)]
struct EnvRecord {
    hex_col: i32,
    hex_row: i32,
    ability: String,
    /// `"player"` | `"enemy"` | absent (= neutral, `None`).
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Deserialize)]
struct VictoryRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    enemy_name: Option<String>,
    #[serde(default)]
    target_name: Option<String>,
    #[serde(default)]
    marker_color: Option<[f32; 3]>,
    #[serde(default)]
    description: Option<String>,
    /// Sub-conditions for `all_of` — recursive TOML inline tables.
    #[serde(default)]
    conditions: Option<Vec<VictoryRecord>>,
}

#[derive(Deserialize)]
struct ObjectiveRecord {
    id: String,
    condition: VictoryRecord,
    #[serde(default)]
    hidden: bool,
}

/// An enemy as it appears in `encounters.toml`.
///
/// If `template` is set, every other scalar/block is optional and falls back
/// to the template's value. Without `template`, the scalars + `stats` +
/// `equipment` blocks are all required (validated at resolution time).
#[derive(Deserialize)]
struct EnemyRecord {
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    race: Option<String>,
    #[serde(default)]
    faction: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    sprite: Option<String>,
    #[serde(default)]
    speed: Option<i32>,
    #[serde(default)]
    stats: Option<StatsRecord>,
    #[serde(default)]
    equipment: Option<EquipmentRecord>,
    #[serde(default)]
    resources: Option<ResourcesRecord>,
    #[serde(default)]
    ability_ids: Option<Vec<String>>,
    hex_col: i32,
    hex_row: i32,
    #[serde(default)]
    phases: Vec<PhaseRecord>,
    #[serde(default)]
    aura: Option<AuraRecord>,
    /// Creature tags (e.g. `["undead", "living"]`).  Parsed into `EnemyDef.tags`.
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct AuraRecord {
    status: String,
    radius: u32,
    #[serde(default = "default_affects")]
    affects: String,
    #[serde(default)]
    affects_tags: Vec<String>,
}

fn default_affects() -> String {
    "enemies".into()
}

#[derive(Deserialize)]
struct PhaseRecord {
    hp_below_pct: i32,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    stats: Option<StatsRecord>,
    #[serde(default)]
    ability_ids: Option<Vec<String>>,
    #[serde(default)]
    heal_to_full: bool,
    #[serde(default)]
    flavor: Option<String>,
    #[serde(default)]
    victory_override: Option<VictoryRecord>,
    #[serde(default)]
    turn_limit: Option<u32>,
    #[serde(default)]
    ai_behavior: Option<AiBehaviorKind>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

// ── Resolution helpers ──────────────────────────────────────────────────────

fn lookup_template<'a>(
    path: &str,
    templates: &'a HashMap<String, UnitTemplateDef>,
    enc_id: &str,
    id: &str,
) -> &'a UnitTemplateDef {
    templates.get(id).unwrap_or_else(|| {
        panic!(
            "{path}: encounter '{enc_id}' references unknown unit_template '{id}' \
             (not in this scenario's merged content view)",
        )
    })
}

fn convert_aura(path: &str, enc_id: &str, a: AuraRecord) -> AuraDef {
    let affects = match a.affects.as_str() {
        "enemies" => AuraAffects::Enemies,
        "allies" => AuraAffects::Allies,
        "all" => AuraAffects::All,
        other => panic!(
            "{path}: encounter '{enc_id}' aura has unknown affects='{other}' (expected enemies|allies|all)",
        ),
    };
    AuraDef {
        status: combat_engine::StatusId::from(a.status.as_str()),
        radius: a.radius,
        affects,
        affects_tags: a
            .affects_tags
            .iter()
            .map(|s| combat_engine::TagId::from(s.as_str()))
            .collect(),
    }
}

fn resolve_phase(
    path: &str,
    enc_id: &str,
    p: PhaseRecord,
    templates: &HashMap<String, UnitTemplateDef>,
) -> PhaseDef {
    let base = p
        .template
        .as_ref()
        .map(|id| lookup_template(path, templates, enc_id, id));

    // Phases fill in only fields explicitly provided OR inherited from template.
    let name = p.name.or_else(|| base.map(|t| t.name.clone()));

    // Block-level overrides: stats (whole block), ability_ids (whole list).
    let stats: Option<CombatStats> = p
        .stats
        .map(Into::into)
        .or_else(|| base.map(|t| t.stats.clone()));
    let ability_ids: Option<Vec<AbilityId>> = p
        .ability_ids
        .map(|v| v.into_iter().map(|s| AbilityId::from(s.as_str())).collect())
        .or_else(|| base.map(|t| t.ability_ids.clone()));

    // Equipment and base_speed: inherited from template when a template is
    // present; None when there is no template (no stat change for that field).
    // PhaseRecord has no inline equipment/speed override fields — template-only.
    let equipment: Option<EquipmentBlock> = base.map(|t| t.equipment.clone());
    let base_speed: Option<i32> = base.map(|t| t.speed);

    PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(p.hp_below_pct),
        name,
        stats,
        ability_ids,
        heal_to_full: p.heal_to_full,
        flavor: p.flavor,
        victory_override: p.victory_override.map(|v| resolve_victory(path, enc_id, v)),
        turn_limit: p.turn_limit,
        ai_behavior: p.ai_behavior,
        tags: p.tags.map(|v| {
            v.into_iter()
                .map(|s| combat_engine::TagId::from(s.as_str()))
                .collect()
        }),
        equipment,
        base_speed,
    }
}

fn resolve_enemy(
    path: &str,
    enc_id: &str,
    rec: EnemyRecord,
    templates: &HashMap<String, UnitTemplateDef>,
) -> EnemyDef {
    let base = rec
        .template
        .as_ref()
        .map(|id| lookup_template(path, templates, enc_id, id));

    // Helper closure: scalar field that must end up Some — overrides win, then
    // template, otherwise panic.
    let require = |label: &str, v: Option<String>, from_template: Option<String>| -> String {
        v.or(from_template).unwrap_or_else(|| panic!(
            "{path}: encounter '{enc_id}' enemy is missing `{label}` and has no template providing it",
        ))
    };

    let name = require("name", rec.name, base.map(|t| t.name.clone()));
    let race = require("race", rec.race, base.map(|t| t.race.clone()));
    let speed = rec
        .speed
        .or_else(|| base.map(|t| t.speed))
        .unwrap_or_else(|| {
            panic!("{path}: encounter '{enc_id}' enemy '{name}' is missing `speed`",)
        });

    // Faction/path: explicit override OR template; no way to unset from template (acceptable).
    let faction = rec.faction.or_else(|| base.and_then(|t| t.faction.clone()));
    let combat_path = rec.path.or_else(|| base.and_then(|t| t.path.clone()));
    let sprite = rec
        .sprite
        .clone()
        .or_else(|| base.and_then(|t| t.sprite.clone()));

    // Block overrides — whole block replaced if present, otherwise taken from template, else panic.
    let stats: CombatStats = rec
        .stats
        .map(Into::into)
        .or_else(|| base.map(|t| t.stats.clone()))
        .unwrap_or_else(|| {
            panic!("{path}: encounter '{enc_id}' enemy '{name}' is missing `stats` block",)
        });
    let equipment = rec
        .equipment
        .map(Into::into)
        .or_else(|| base.map(|t| t.equipment.clone()))
        .unwrap_or_else(|| {
            panic!("{path}: encounter '{enc_id}' enemy '{name}' is missing `equipment` block",)
        });
    let resources: ResourcesBlock = rec
        .resources
        .map(Into::into)
        .or_else(|| base.map(|t| t.resources.clone()))
        .unwrap_or_default();

    let ability_ids: Vec<AbilityId> = rec
        .ability_ids
        .map(|v| v.into_iter().map(|s| AbilityId::from(s.as_str())).collect())
        .or_else(|| base.map(|t| t.ability_ids.clone()))
        .unwrap_or_else(|| {
            panic!("{path}: encounter '{enc_id}' enemy '{name}' is missing `ability_ids`",)
        });

    EnemyDef {
        name,
        race,
        faction,
        path: combat_path,
        sprite,
        stats,
        speed,
        main_hand: equipment.main_hand,
        off_hand: equipment.off_hand,
        chest: equipment.chest,
        legs: equipment.legs,
        feet: equipment.feet,
        ability_ids,
        rage_max: resources.rage_max,
        mana_max: resources.mana_max,
        energy_max: resources.energy_max,
        hex_pos: crate::game::hex::hex_from_offset(rec.hex_col, rec.hex_row),
        phases: rec
            .phases
            .into_iter()
            .map(|p| resolve_phase(path, enc_id, p, templates))
            .collect(),
        aura: rec.aura.map(|a| convert_aura(path, enc_id, a)),
        tags: rec
            .tags
            .iter()
            .map(|s| combat_engine::TagId::from(s.as_str()))
            .collect(),
    }
}

/// Recursively resolve a `VictoryRecord` into a `VictoryCondition`.
fn resolve_victory(path: &str, enc_id: &str, v: VictoryRecord) -> VictoryCondition {
    match v.kind.as_str() {
        "all_enemies_dead" => VictoryCondition::AllEnemiesDead,
        "kill_target" => VictoryCondition::KillTarget {
            enemy_name: v.enemy_name.unwrap_or_else(|| {
                panic!("{path}: encounter '{enc_id}' victory=kill_target missing enemy_name",)
            }),
            marker_color: v.marker_color.unwrap_or(DEFAULT_TARGET_MARKER),
            description: v.description,
        },
        "keep_alive" => VictoryCondition::KeepAlive {
            target_name: v.target_name.unwrap_or_else(|| {
                panic!("{path}: encounter '{enc_id}' victory=keep_alive missing target_name",)
            }),
            marker_color: v.marker_color.unwrap_or(DEFAULT_TARGET_MARKER),
        },
        "all_of" => {
            let sub = v.conditions.unwrap_or_default();
            VictoryCondition::AllOf(
                sub.into_iter()
                    .map(|c| resolve_victory(path, enc_id, c))
                    .collect(),
            )
        }
        other => panic!("{path}: encounter '{enc_id}' has unknown victory type '{other}'",),
    }
}

/// Parse an `encounters.toml` body. `path` scopes error messages. Template refs
/// resolve against the scenario's already-merged unit template map (`templates`).
pub fn load_encounters_from_str(
    _scenario_id: &str,
    path: &str,
    src: &str,
    templates: &HashMap<String, UnitTemplateDef>,
) -> Vec<EncounterDef> {
    let file: EncounterFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));

    file.encounters
        .into_iter()
        .map(|enc| {
            let on_defeat = match enc.on_defeat.as_deref() {
                None | Some("retry") => OnDefeat::Retry,
                Some("proceed") => OnDefeat::Proceed,
                Some(other) => panic!(
                    "{path}: encounter '{}' has unknown on_defeat '{other}' \
                     (expected retry|proceed)",
                    enc.id,
                ),
            };
            let objectives: Vec<ObjectiveDef> = enc
                .objectives
                .into_iter()
                .map(|o| ObjectiveDef {
                    id: o.id,
                    condition: resolve_victory(path, &enc.id, o.condition),
                    hidden: o.hidden,
                })
                .collect();
            EncounterDef {
                id: enc.id.clone(),
                name: enc.name.clone(),
                victory: match enc.victory {
                    None => VictoryCondition::AllEnemiesDead,
                    Some(v) => resolve_victory(path, &enc.id, v),
                },
                enemies: enc
                    .enemies
                    .into_iter()
                    .map(|e| resolve_enemy(path, &enc.id, e, templates))
                    .collect(),
                obstacles: enc
                    .obstacles
                    .into_iter()
                    .map(|o| crate::game::hex::hex_from_offset(o.hex_col, o.hex_row))
                    .collect(),
                environment: enc
                    .environment
                    .into_iter()
                    .map(|e| {
                        let owner = match e.owner.as_deref() {
                            None => None,
                            Some("player") => Some(combat_engine::state::Team::Player),
                            Some("enemy") => Some(combat_engine::state::Team::Enemy),
                            Some(other) => panic!(
                                "{path}: encounter '{}' has unknown env owner '{other}' \
                                 (must be \"player\", \"enemy\", or absent)",
                                enc.id,
                            ),
                        };
                        EnvObjectDef {
                            hex: crate::game::hex::hex_from_offset(e.hex_col, e.hex_row),
                            ability: AbilityId::from(e.ability.as_str()),
                            owner,
                        }
                    })
                    .collect(),
                on_defeat,
                objectives,
                rewards: enc.rewards,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_record_resolves_victory_override_and_turn_limit() {
        let toml_src = r#"
hp_below_pct = 50
name = "Phase Two"
heal_to_full = false
turn_limit = 3

[victory_override]
type = "kill_target"
enemy_name = "Phase Two"
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");

        let phase = resolve_phase("test", "enc1", record, &Default::default());

        assert_eq!(phase.turn_limit, Some(3));
        let ov = phase
            .victory_override
            .expect("victory_override must be Some");
        match ov {
            VictoryCondition::KillTarget { enemy_name, .. } => {
                assert_eq!(enemy_name, "Phase Two");
            }
            other => panic!("expected KillTarget, got {other:?}"),
        }
    }

    /// A `PhaseDef` with no `victory_override`/`turn_limit` resolves to `None` for both.
    #[test]
    fn phase_record_without_override_fields_resolves_to_none() {
        let toml_src = r#"
hp_below_pct = 75
heal_to_full = false
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");
        let phase = resolve_phase("test", "enc1", record, &Default::default());
        assert!(phase.victory_override.is_none());
        assert!(phase.turn_limit.is_none());
    }

    /// A `PhaseRecord` with `ai_behavior = "flee"` resolves to `Some(AiBehaviorKind::Flee)`.
    #[test]
    fn phase_record_with_ai_behavior_flee_resolves_correctly() {
        let toml_src = r#"
hp_below_pct = 50
heal_to_full = false
ai_behavior = "flee"
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");
        let phase = resolve_phase("test", "enc1", record, &Default::default());
        assert_eq!(phase.ai_behavior, Some(AiBehaviorKind::Flee));
    }

    /// A `PhaseRecord` without `ai_behavior` resolves to `None` (additive default).
    #[test]
    fn phase_record_without_ai_behavior_resolves_to_none() {
        let toml_src = r#"
hp_below_pct = 75
heal_to_full = false
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");
        let phase = resolve_phase("test", "enc1", record, &Default::default());
        assert!(phase.ai_behavior.is_none());
    }

    /// A `PhaseRecord` with `tags = ["aberration", "incorporeal"]` resolves to
    /// `PhaseDef.tags == Some({aberration, incorporeal})`.
    #[test]
    fn phase_record_with_tags_resolves_correctly() {
        let toml_src = r#"
hp_below_pct = 50
heal_to_full = false
tags = ["aberration", "incorporeal"]
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");
        let phase = resolve_phase("test", "enc1", record, &Default::default());
        let tags = phase.tags.expect("tags must be Some when declared in TOML");
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&combat_engine::TagId::from("aberration")));
        assert!(tags.contains(&combat_engine::TagId::from("incorporeal")));
    }

    /// A `PhaseRecord` without `tags` resolves to `PhaseDef.tags == None`
    /// (absent tags field ⇒ keep current — no replacement).
    #[test]
    fn phase_record_without_tags_resolves_to_none() {
        let toml_src = r#"
hp_below_pct = 75
heal_to_full = false
"#;
        let record: PhaseRecord =
            toml::from_str(toml_src).expect("PhaseRecord must deserialize from TOML");
        let phase = resolve_phase("test", "enc1", record, &Default::default());
        assert!(
            phase.tags.is_none(),
            "absent tags field must resolve to None"
        );
    }

    // ── T5: EnvObjectDef.owner from TOML ─────────────────────────────────────

    /// Base encounter TOML (no environment entry) — parses cleanly.
    const BASE_ENC_TOML: &str = r#"
[[encounters]]
id = "enc1"
name = "Test"

[[encounters.enemies]]
name = "Goblin"
race = "goblin"
speed = 3
hex_col = 5
hex_row = 5
ability_ids = []

[encounters.enemies.stats]
max_hp = 10
strength = 5
dexterity = 5
constitution = 5
intelligence = 0
wisdom = 5
charisma = 5

[encounters.enemies.equipment]
main_hand = "unarmed"
chest = "cloth"
legs = "cloth"
feet = "cloth"
"#;

    /// Append an environment entry with the given extra fields.
    fn env_encounter_toml(owner_field: &str) -> String {
        format!(
            "{BASE_ENC_TOML}\n[[encounters.environment]]\nhex_col = 2\nhex_row = 3\nability = \"spike_trap\"\n{owner_field}\n"
        )
    }

    fn load_env(toml_src: &str) -> EnvObjectDef {
        let encs = load_encounters_from_str("test_id", "test.toml", toml_src, &Default::default());
        encs.into_iter()
            .next()
            .unwrap()
            .environment
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn parses_env_owner_player() {
        let src = env_encounter_toml(r#"owner = "player""#);
        let def = load_env(&src);
        assert_eq!(def.owner, Some(combat_engine::state::Team::Player));
    }

    #[test]
    fn parses_env_owner_enemy() {
        let src = env_encounter_toml(r#"owner = "enemy""#);
        let def = load_env(&src);
        assert_eq!(def.owner, Some(combat_engine::state::Team::Enemy));
    }

    #[test]
    fn env_owner_absent_is_neutral_none() {
        let src = env_encounter_toml("");
        let def = load_env(&src);
        assert_eq!(def.owner, None);
    }

    #[test]
    #[should_panic(expected = "unknown env owner 'wizard'")]
    fn env_owner_unknown_string_panics() {
        let src = env_encounter_toml(r#"owner = "wizard""#);
        load_env(&src);
    }

    // ── on_defeat + objectives ───────────────────────────────────────────────

    fn load_enc(toml_src: &str) -> EncounterDef {
        load_encounters_from_str("test_id", "test.toml", toml_src, &Default::default())
            .into_iter()
            .next()
            .unwrap()
    }

    /// Build a complete encounters TOML with optional extra fields injected
    /// immediately after the `[[encounters]]` header line (before sub-tables).
    fn enc_toml_with(extra_top_fields: &str, extra_suffix: &str) -> String {
        format!(
            r#"
[[encounters]]
id = "enc1"
name = "Test"
{extra_top_fields}
[[encounters.enemies]]
name = "Goblin"
race = "goblin"
speed = 3
hex_col = 5
hex_row = 5
ability_ids = []

[encounters.enemies.stats]
max_hp = 10
strength = 5
dexterity = 5
constitution = 5
intelligence = 0
wisdom = 5
charisma = 5

[encounters.enemies.equipment]
main_hand = "unarmed"
chest = "cloth"
legs = "cloth"
feet = "cloth"
{extra_suffix}
"#
        )
    }

    #[test]
    fn on_defeat_absent_defaults_to_retry() {
        let enc = load_enc(BASE_ENC_TOML);
        assert_eq!(enc.on_defeat, OnDefeat::Retry);
    }

    #[test]
    fn on_defeat_proceed_parses() {
        let src = enc_toml_with(r#"on_defeat = "proceed""#, "");
        let enc = load_enc(&src);
        assert_eq!(enc.on_defeat, OnDefeat::Proceed);
    }

    #[test]
    #[should_panic(expected = "unknown on_defeat 'bogus'")]
    fn on_defeat_unknown_string_panics() {
        let src = enc_toml_with(r#"on_defeat = "bogus""#, "");
        load_enc(&src);
    }

    // ── objectives ───────────────────────────────────────────────────────────

    #[test]
    fn objectives_absent_is_empty() {
        let enc = load_enc(BASE_ENC_TOML);
        assert!(enc.objectives.is_empty());
    }

    #[test]
    fn objectives_parse_keep_alive() {
        let suffix = r#"
[[encounters.objectives]]
id = "boat_saved"
hidden = true
[encounters.objectives.condition]
type = "keep_alive"
target_name = "Лодка"
"#;
        let src = enc_toml_with("", suffix);
        let enc = load_enc(&src);
        assert_eq!(enc.objectives.len(), 1);
        let obj = &enc.objectives[0];
        assert_eq!(obj.id, "boat_saved");
        assert!(obj.hidden);
        match &obj.condition {
            VictoryCondition::KeepAlive { target_name, .. } => {
                assert_eq!(target_name, "Лодка");
            }
            other => panic!("expected KeepAlive, got {other:?}"),
        }
    }

    #[test]
    fn rewards_absent_defaults_to_empty() {
        let enc = load_enc(&enc_toml_with("", ""));
        assert!(enc.rewards.is_empty());
    }

    #[test]
    fn rewards_present_parsed() {
        let enc = load_enc(&enc_toml_with(r#"rewards = ["sword_x", "plate_y"]"#, ""));
        assert_eq!(enc.rewards, vec!["sword_x", "plate_y"]);
    }

    // ── sprite precedence: record > template > None ──────────────────────────

    fn enemy_record(extra: &str) -> EnemyRecord {
        let toml_src = format!(
            r#"
template = "imp"
hex_col = 1
hex_row = 1
{extra}
"#
        );
        toml::from_str(&toml_src).expect("EnemyRecord must deserialize")
    }

    fn template_map(sprite: Option<&str>) -> HashMap<String, UnitTemplateDef> {
        let tpl_src = format!(
            r#"
[[unit_templates]]
id = "imp"
name = "Imp"
race = "imp"
speed = 4
ability_ids = []
{}

[unit_templates.stats]
max_hp = 8
strength = 2
dexterity = 5
constitution = 8
intelligence = 0
wisdom = 5
charisma = 5

[unit_templates.equipment]
main_hand = "unarmed"
chest = "cloth"
legs = "cloth"
feet = "cloth"
"#,
            sprite.map_or(String::new(), |s| format!(r#"sprite = "{s}""#))
        );
        crate::content::unit_templates::parse_unit_templates("test.toml", &tpl_src)
            .into_iter()
            .map(|t| (t.id.clone(), t))
            .collect()
    }

    #[test]
    fn resolve_enemy_sprite_precedence() {
        // (record sprite, template sprite) -> expected resolved sprite
        let cases = [
            (
                Some(r#"sprite = "units/r.png""#),
                Some("units/t.png"),
                Some("units/r.png"),
            ),
            (None, Some("units/t.png"), Some("units/t.png")),
            (None, None, None),
        ];
        for (rec_sprite, tpl_sprite, expected) in cases {
            let rec = enemy_record(rec_sprite.unwrap_or(""));
            let templates = template_map(tpl_sprite);
            let resolved = resolve_enemy("test.toml", "enc1", rec, &templates);
            assert_eq!(resolved.sprite.as_deref(), expected);
        }
    }
}
