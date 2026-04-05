use serde::Deserialize;
use crate::core::{AbilityId, DiceExpr, StatusId};

// ── Ability IDs ───────────────────────────────────────────────────────────────

pub const ABILITY_SWORD_ATTACK: AbilityId = AbilityId(1);
pub const ABILITY_SHIELD_BLOCK: AbilityId = AbilityId(2);

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
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
    /// Uses the actor's equipped weapon dice + CombatStats.damage.
    WeaponAttack,
    /// Fixed dice roll + CombatStats.damage (weapon-independent).
    Damage { dice: DiceExpr },
    /// Apply a status to the target.
    ApplyStatus { status: StatusId, duration_rounds: u32 },
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

#[derive(Deserialize)]
struct AbilityRecord {
    id:              u32,
    name:            String,
    target_type:     String,
    effect:          String,
    // Damage fields
    dice_count:      Option<u32>,
    dice_sides:      Option<u32>,
    // ApplyStatus fields
    status_id:       Option<u32>,
    duration_rounds: Option<u32>,
}

const ABILITIES_PATH: &str = "assets/data/abilities.toml";

pub fn load_abilities() -> Vec<AbilityDef> {
    let src = std::fs::read_to_string(ABILITIES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ABILITIES_PATH}: {e}"));

    let file: AbilityFile = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("Cannot parse {ABILITIES_PATH}: {e}"));

    file.abilities
        .into_iter()
        .map(|r| {
            let target_type = match r.target_type.as_str() {
                "single_enemy" => TargetType::SingleEnemy,
                "myself"       => TargetType::Myself,
                other => panic!("{ABILITIES_PATH}: unknown target_type '{other}'"),
            };

            let effect = match r.effect.as_str() {
                "weapon_attack" => EffectDef::WeaponAttack,
                "damage" => EffectDef::Damage {
                    dice: DiceExpr::new(
                        r.dice_count.unwrap_or_else(|| panic!("ability {} missing dice_count", r.id)),
                        r.dice_sides.unwrap_or_else(|| panic!("ability {} missing dice_sides", r.id)),
                        0,
                    ),
                },
                "apply_status" => EffectDef::ApplyStatus {
                    status: StatusId(r.status_id.unwrap_or_else(|| panic!("ability {} missing status_id", r.id))),
                    duration_rounds: r.duration_rounds.unwrap_or_else(|| panic!("ability {} missing duration_rounds", r.id)),
                },
                other => panic!("{ABILITIES_PATH}: unknown effect '{other}'"),
            };

            AbilityDef { id: AbilityId(r.id), name: r.name, target_type, effect }
        })
        .collect()
}
