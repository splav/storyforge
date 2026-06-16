use combat_engine::{DiceExpr, StatusId};
use serde::Deserialize;

/// Semantic class of a buff for saturation-penalty tracking. Two buffs of the
/// same class on the same target don't stack meaningfully — the AI penalises
/// plans that re-apply an already-present class.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuffClass {
    Haste,
    ArmorBuff,
    MagicResistBuff,
    DamageUp,
    Shield,
}

/// Bridge status definition: adds bridge-only metadata around the engine
/// `combat_engine::StatusDef`. `Deref` exposes engine fields directly
/// (`def.armor_bonus`, `def.skips_turn`, …).
#[derive(Debug, Clone)]
pub struct StatusDef {
    // ── metadata (bridge-only) ────────────────────────────────────────────
    pub id: StatusId,
    pub name: String,
    pub dot_dice: Option<DiceExpr>,
    pub ai_controlled: bool, // pact: AI управляет персонажем
    /// AI buff-class for saturation tracking. `None` = not a tracked buff.
    pub buff_class: Option<BuffClass>,
    // ── gameplay (engine) ─────────────────────────────────────────────────
    pub engine: combat_engine::StatusDef,
}

impl From<&StatusDef> for combat_engine::StatusDef {
    fn from(d: &StatusDef) -> Self {
        d.engine
    }
}

impl std::ops::Deref for StatusDef {
    type Target = combat_engine::StatusDef;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}

impl std::ops::DerefMut for StatusDef {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.engine
    }
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StatusFile {
    statuses: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id: String,
    name: String,
    #[serde(default)]
    armor_bonus: i32,
    #[serde(default)]
    magic_resist_bonus: i32,
    #[serde(default)]
    skips_turn: bool,
    #[serde(default)]
    forces_targeting: bool,
    #[serde(default)]
    dot_count: Option<u32>,
    #[serde(default)]
    dot_sides: Option<u32>,
    #[serde(default)]
    blocks_mana_abilities: bool,
    #[serde(default)]
    speed_bonus: i32,
    #[serde(default)]
    hp_percent_dot: i32,
    #[serde(default)]
    heal_per_tick: i32,
    #[serde(default)]
    ai_controlled: bool,
    #[serde(default)]
    causes_disadvantage: bool,
    #[serde(default)]
    buff_class: Option<String>,
}

pub const STATUSES_FILE: &str = "statuses.toml";

pub fn load_statuses() -> Vec<StatusDef> {
    let path = format!("assets/data/{STATUSES_FILE}");
    if !std::path::Path::new(&path).is_file() {
        return Vec::new();
    }
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_statuses(&path, &src)
}

pub fn parse_statuses(path: &str, src: &str) -> Vec<StatusDef> {
    let file: StatusFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));
    file.statuses
        .into_iter()
        .map(|r| {
            let dot_dice = match (r.dot_count, r.dot_sides) {
                (Some(count), Some(sides)) => Some(DiceExpr::new(count, sides, 0)),
                _ => None,
            };
            let buff_class = r.buff_class.as_deref().and_then(|s| match s {
                "haste" => Some(BuffClass::Haste),
                "armor_buff" => Some(BuffClass::ArmorBuff),
                "magic_resist_buff" => Some(BuffClass::MagicResistBuff),
                "damage_up" => Some(BuffClass::DamageUp),
                "shield" => Some(BuffClass::Shield),
                other => {
                    eprintln!("statuses.toml: unknown buff_class '{other}' on '{}'", r.id);
                    None
                }
            });
            StatusDef {
                id: StatusId::from(r.id.as_str()),
                name: r.name,
                dot_dice,
                ai_controlled: r.ai_controlled,
                buff_class,
                engine: combat_engine::StatusDef {
                    causes_disadvantage: r.causes_disadvantage,
                    blocks_mana_abilities: r.blocks_mana_abilities,
                    forces_targeting: r.forces_targeting,
                    skips_turn: r.skips_turn,
                    bonuses: combat_engine::StatusBonuses {
                        runtime: combat_engine::RuntimeStatsDelta(combat_engine::RuntimeStats {
                            armor: r.armor_bonus,
                            magic_resist: r.magic_resist_bonus,
                            base_speed: r.speed_bonus,
                        }),
                    },
                    // Mirror of the bridge `dot_dice` (Copy): the engine copy drives the
                    // cast-time roll into `dot_per_tick`; the bridge copy feeds the UI tint.
                    dot_dice,
                    hp_percent_dot: r.hp_percent_dot,
                    heal_per_tick: r.heal_per_tick,
                },
            }
        })
        .collect()
}
