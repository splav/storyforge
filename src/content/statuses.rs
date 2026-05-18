use crate::core::{DiceExpr, StatusId};
use serde::Deserialize;

/// Semantic class of a buff for saturation-penalty tracking. Two buffs of the
/// same class on the same target don't stack meaningfully — the AI penalises
/// plans that re-apply an already-present class.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuffClass {
    Haste,
    ArmorBuff,
    DamageUp,
    Shield,
}

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub id: StatusId,
    pub name: String,
    pub armor_bonus: i32,        // снижает урон от физических атак
    pub damage_taken_bonus: i32, // увеличивает весь получаемый урон (применяется после брони)
    pub skips_turn: bool,
    pub forces_targeting: bool,  // враги вынуждены атаковать цель с этим статусом
    pub dot_dice: Option<DiceExpr>, // кубик урона за тик (бросается один раз при наложении)
    pub blocks_mana_abilities: bool, // faith: запрет способностей с маной
    pub speed_bonus: i32,            // heritage: модификатор скорости
    pub hp_percent_dot: i32,         // heritage: % от max_hp урона за тик (ceil)
    pub ai_controlled: bool,         // pact: AI управляет персонажем
    pub causes_disadvantage: bool,   // носитель бросает все броски с disadvantage
    /// AI buff-class for saturation tracking. `None` = not a tracked buff.
    pub buff_class: Option<BuffClass>,
}

impl From<&StatusDef> for combat_engine::StatusDef {
    fn from(d: &StatusDef) -> Self {
        combat_engine::StatusDef {
            causes_disadvantage: d.causes_disadvantage,
            blocks_mana_abilities: d.blocks_mana_abilities,
            forces_targeting: d.forces_targeting,
            skips_turn: d.skips_turn,
            armor_bonus: d.armor_bonus,
            damage_taken_bonus: d.damage_taken_bonus,
            speed_bonus: d.speed_bonus,
            hp_percent_dot: d.hp_percent_dot,
        }
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
    damage_taken_bonus: i32,
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
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
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
                "haste"      => Some(BuffClass::Haste),
                "armor_buff" => Some(BuffClass::ArmorBuff),
                "damage_up"  => Some(BuffClass::DamageUp),
                "shield"     => Some(BuffClass::Shield),
                other => {
                    eprintln!("statuses.toml: unknown buff_class '{other}' on '{}'", r.id);
                    None
                }
            });
            StatusDef {
                id: StatusId::from(r.id.as_str()),
                name: r.name,
                armor_bonus: r.armor_bonus,
                damage_taken_bonus: r.damage_taken_bonus,
                skips_turn: r.skips_turn,
                forces_targeting: r.forces_targeting,
                dot_dice,
                blocks_mana_abilities: r.blocks_mana_abilities,
                speed_bonus: r.speed_bonus,
                hp_percent_dot: r.hp_percent_dot,
                ai_controlled: r.ai_controlled,
                causes_disadvantage: r.causes_disadvantage,
                buff_class,
            }
        })
        .collect()
}
