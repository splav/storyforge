use crate::core::{AbilityId, DiceExpr, StatusId};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    SingleAlly,
    Myself,
}

/// To whom a status is applied when the ability resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusOn {
    /// The ability's resolved target (enemy, ally, or self depending on target_type).
    Target,
    /// Always the actor who used the ability.
    MySelf,
}

#[derive(Debug, Clone)]
pub struct StatusApplication {
    pub status: StatusId,
    pub duration_rounds: u32,
    pub on: StatusOn,
}

#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id: AbilityId,
    pub name: String,
    pub target_type: TargetType,
    /// Max range in hex steps. 0 = ignored (for Myself target_type).
    pub range: u32,
    pub effect: EffectDef,
    pub rage_cost: i32,
    pub mana_cost: i32,
    /// Status effects applied when the ability resolves.
    pub statuses: Vec<StatusApplication>,
}

#[derive(Debug, Clone)]
pub enum EffectDef {
    /// No direct damage or heal — ability only applies statuses.
    None,
    WeaponAttack,
    Damage {
        dice: DiceExpr,
    },
    /// spell_power + intelligence + dice, bypasses armor
    SpellDamage {
        dice: DiceExpr,
    },
    /// spell_power + intelligence + dice, heals target (capped at max_hp)
    Heal {
        dice: DiceExpr,
    },
    /// Grants bonus movement to the actor. Does NOT end the turn.
    GrantMovement {
        distance: i32,
    },
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

#[derive(Deserialize)]
struct AbilityRecord {
    id: String,
    name: String,
    target_type: String,
    #[serde(default)]
    effect: String,
    #[serde(default)]
    range: u32,
    dice_count: Option<u32>,
    dice_sides: Option<u32>,
    #[serde(default)]
    distance: i32,
    #[serde(default)]
    rage_cost: i32,
    #[serde(default)]
    mana_cost: i32,
    #[serde(default)]
    statuses: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id: String,
    on: String,
    duration: u32,
}

const ABILITIES_PATH: &str = "assets/data/abilities.toml";

pub fn load_abilities() -> Vec<AbilityDef> {
    let src = std::fs::read_to_string(ABILITIES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ABILITIES_PATH}: {e}"));
    let file: AbilityFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {ABILITIES_PATH}: {e}"));

    file.abilities
        .into_iter()
        .map(|r| {
            let target_type = match r.target_type.as_str() {
                "single_enemy" => TargetType::SingleEnemy,
                "single_ally" => TargetType::SingleAlly,
                "myself" => TargetType::Myself,
                other => panic!("{ABILITIES_PATH}: unknown target_type '{other}'"),
            };
            let need_dice = |id: &str, count: Option<u32>, sides: Option<u32>| {
                DiceExpr::new(
                    count.unwrap_or_else(|| panic!("ability '{id}' missing dice_count")),
                    sides.unwrap_or_else(|| panic!("ability '{id}' missing dice_sides")),
                    0,
                )
            };
            let effect = match r.effect.as_str() {
                "" | "none" => EffectDef::None,
                "weapon_attack" => EffectDef::WeaponAttack,
                "damage" => EffectDef::Damage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "spell_damage" => EffectDef::SpellDamage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "heal" => EffectDef::Heal {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "grant_movement" => EffectDef::GrantMovement {
                    distance: r.distance,
                },
                other => panic!("{ABILITIES_PATH}: unknown effect '{other}'"),
            };
            let statuses = r
                .statuses
                .into_iter()
                .map(|s| {
                    let on = match s.on.as_str() {
                        "target" => StatusOn::Target,
                        "self" => StatusOn::MySelf,
                        other => panic!("{ABILITIES_PATH}: unknown status 'on' value '{other}'"),
                    };
                    StatusApplication {
                        status: StatusId::from(s.id.as_str()),
                        duration_rounds: s.duration,
                        on,
                    }
                })
                .collect();
            AbilityDef {
                id: AbilityId::from(r.id.as_str()),
                name: r.name,
                target_type,
                range: r.range,
                effect,
                rage_cost: r.rage_cost,
                mana_cost: r.mana_cost,
                statuses,
            }
        })
        .collect()
}
