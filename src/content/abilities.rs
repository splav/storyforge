use crate::core::{AbilityId, DiceExpr, StatusId};
use crate::content::statuses::STATUS_DEFENDING;

// ── Ability IDs ───────────────────────────────────────────────────────────────

pub const ABILITY_SWORD_ATTACK:  AbilityId = AbilityId(1);
pub const ABILITY_SHIELD_BLOCK:  AbilityId = AbilityId(2);
pub const ABILITY_GOBLIN_ATTACK: AbilityId = AbilityId(3);

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    Myself,
}

#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id:          AbilityId,
    pub name:        &'static str,
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

// ── Catalogue ─────────────────────────────────────────────────────────────────

pub fn default_abilities() -> Vec<AbilityDef> {
    vec![
        AbilityDef {
            id:          ABILITY_SWORD_ATTACK,
            name:        "Атака мечом",
            target_type: TargetType::SingleEnemy,
            effect:      EffectDef::WeaponAttack,
        },
        AbilityDef {
            id:          ABILITY_SHIELD_BLOCK,
            name:        "Блок щитом",
            target_type: TargetType::Myself,
            effect:      EffectDef::ApplyStatus { status: STATUS_DEFENDING, duration_rounds: 1 },
        },
        AbilityDef {
            id:          ABILITY_GOBLIN_ATTACK,
            name:        "Удар гоблина",
            target_type: TargetType::SingleEnemy,
            effect:      EffectDef::WeaponAttack,
        },
    ]
}
