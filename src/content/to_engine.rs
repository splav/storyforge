//! Bevy content → `combat_engine` type adapters.
//!
//! This module is the canonical place for translating Bevy/content-layer types
//! (`CritFailEffect`, `AbilityDef`, `StatusDef`) into their pure-engine
//! equivalents.  Both `engine_bridge` (ECS path) and `combat/ai/plan/sim`
//! (AI simulation path) delegate to these helpers so the mapping logic lives
//! in exactly one place.
//!
//! **No Bevy dependencies here** — only `crate::content::*` and `combat_engine`.

use crate::content::abilities::{AbilityDef, EffectDef, StatusOn, TargetType};
use crate::content::races::CritFailEffect;
use crate::content::statuses::StatusDef;
use combat_engine::{
    AoEShape as EngineAoEShape, Cost as EngineCost, EffectDef as EngineEffectDef,
    StatusApplication as EngineStatusApplication, StatusOn as EngineStatusOn,
};

/// Translate a Bevy `CritFailEffect` into the engine's `CritFailOutcome`.
///
/// `CircuitBreach` uses a fixed `SelfDamage(0d1+2)` placeholder (Phase 2 step 6f).
/// Full mana_cost-derived damage parity is a Phase 2 step 7 follow-up.
pub fn crit_fail_outcome(e: &CritFailEffect) -> combat_engine::CritFailOutcome {
    use CritFailEffect::*;
    use combat_engine::CritFailOutcome as Out;
    use combat_engine::{DiceExpr, StatusId};
    match e {
        Miss => Out::Miss,
        ManaOverload => Out::DoubleCost,
        BrokenFaith => Out::ApplyStatus(StatusId::from("broken_faith")),
        CircuitBreach => Out::SelfDamage(DiceExpr::new(0, 1, 2)), // placeholder; step 7 refines
        Exhaustion => Out::ApplyStatus(StatusId::from("exhaustion")),
        PactControl => Out::ApplyStatus(StatusId::from("pact_control")),
    }
}

/// Translate a Bevy `AbilityDef` into a `combat_engine::AbilityDef`.
pub fn ability_def(def: &AbilityDef) -> combat_engine::AbilityDef {
    combat_engine::AbilityDef {
        key: def.key.clone(),
        cost_ap: def.cost_ap,
        costs: def
            .costs
            .iter()
            .map(|c| EngineCost { resource: c.resource, amount: c.amount })
            .collect(),
        range: combat_engine::AbilityRange { min: def.range.min, max: def.range.max },
        target_type: match def.target_type {
            TargetType::SingleEnemy => combat_engine::TargetType::SingleEnemy,
            TargetType::SingleAlly => combat_engine::TargetType::SingleAlly,
            TargetType::Myself => combat_engine::TargetType::Myself,
            TargetType::Ground => combat_engine::TargetType::Ground,
        },
        aoe: match def.aoe {
            crate::content::abilities::AoEShape::None => EngineAoEShape::None,
            crate::content::abilities::AoEShape::Circle { radius } => {
                EngineAoEShape::Circle { radius }
            }
            crate::content::abilities::AoEShape::Line { length } => {
                EngineAoEShape::Line { length }
            }
        },
        friendly_fire: def.friendly_fire,
        effect: match &def.effect {
            EffectDef::None => EngineEffectDef::None,
            EffectDef::WeaponAttack => EngineEffectDef::WeaponAttack,
            EffectDef::Damage { dice } => EngineEffectDef::Damage { dice: *dice },
            EffectDef::SpellDamage { dice } => EngineEffectDef::SpellDamage { dice: *dice },
            EffectDef::Heal { dice } => EngineEffectDef::Heal { dice: *dice },
            EffectDef::GrantMovement { distance } => {
                EngineEffectDef::GrantMovement { distance: *distance }
            }
            EffectDef::RestoreResources => EngineEffectDef::RestoreResources,
            // Summon is out of engine scope in Phase 2.
            EffectDef::Summon { .. } => EngineEffectDef::None,
        },
        statuses: def
            .statuses
            .iter()
            .map(|s| EngineStatusApplication {
                status: s.status.clone(),
                duration_rounds: s.duration_rounds,
                on: match s.on {
                    StatusOn::Target => EngineStatusOn::Target,
                    StatusOn::MySelf => EngineStatusOn::MySelf,
                },
            })
            .collect(),
    }
}

/// Translate a Bevy `StatusDef` into a `combat_engine::StatusDef`.
pub fn status_def(def: &StatusDef) -> combat_engine::StatusDef {
    combat_engine::StatusDef {
        causes_disadvantage: def.causes_disadvantage,
        blocks_mana_abilities: def.blocks_mana_abilities,
        forces_targeting: def.forces_targeting,
        skips_turn: def.skips_turn,
        bonuses: combat_engine::StatusBonuses {
            armor_bonus: def.bonuses.armor_bonus,
            damage_taken_bonus: def.bonuses.damage_taken_bonus,
            speed_bonus: def.bonuses.speed_bonus,
        },
        hp_percent_dot: def.hp_percent_dot,
    }
}
