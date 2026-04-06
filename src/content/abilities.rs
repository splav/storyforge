use serde::Deserialize;
use crate::core::{AbilityId, DiceExpr, StatusId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    SingleAlly,
    Myself,
}

#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id:          AbilityId,
    pub name:        String,
    pub target_type: TargetType,
    pub effect:      EffectDef,
}

#[derive(Debug, Clone)]
pub enum EffectDef {
    WeaponAttack,
    Damage { dice: DiceExpr },
    ApplyStatus { status: StatusId, duration_rounds: u32 },
    /// spell_power + intelligence + dice
    SpellDamage { dice: DiceExpr },
    /// spell_power + intelligence + dice, heals target (capped at max_hp)
    Heal { dice: DiceExpr },
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

#[derive(Deserialize)]
struct AbilityRecord {
    id:              String,
    name:            String,
    target_type:     String,
    effect:          String,
    dice_count:      Option<u32>,
    dice_sides:      Option<u32>,
    status_id:       Option<String>,
    duration_rounds: Option<u32>,
}

const ABILITIES_PATH: &str = "assets/data/abilities.toml";

pub fn load_abilities() -> Vec<AbilityDef> {
    let src = std::fs::read_to_string(ABILITIES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ABILITIES_PATH}: {e}"));
    let file: AbilityFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {ABILITIES_PATH}: {e}"));

    file.abilities.into_iter().map(|r| {
        let target_type = match r.target_type.as_str() {
            "single_enemy" => TargetType::SingleEnemy,
            "single_ally"  => TargetType::SingleAlly,
            "myself"       => TargetType::Myself,
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
            "weapon_attack" => EffectDef::WeaponAttack,
            "damage" => EffectDef::Damage {
                dice: need_dice(&r.id, r.dice_count, r.dice_sides),
            },
            "apply_status" => EffectDef::ApplyStatus {
                status: StatusId::from(r.status_id.unwrap_or_else(|| panic!("ability '{}' missing status_id", r.id)).as_str()),
                duration_rounds: r.duration_rounds.unwrap_or_else(|| panic!("ability '{}' missing duration_rounds", r.id)),
            },
            "spell_damage" => EffectDef::SpellDamage {
                dice: need_dice(&r.id, r.dice_count, r.dice_sides),
            },
            "heal" => EffectDef::Heal {
                dice: need_dice(&r.id, r.dice_count, r.dice_sides),
            },
            other => panic!("{ABILITIES_PATH}: unknown effect '{other}'"),
        };
        AbilityDef { id: AbilityId::from(r.id.as_str()), name: r.name, target_type, effect }
    }).collect()
}
