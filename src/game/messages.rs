//! Combat messages.
//!
//! Input → UseAbility → validation → resolution → ApplyDamage/ApplyStatus → EndTurn
//! Each step is a separate system reacting to these messages.

use crate::core::{AbilityId, StatusId};
use bevy::prelude::*;

#[derive(Message)]
pub struct StartCombat {
    pub encounter: Entity,
}

#[derive(Message, Clone)]
pub struct UseAbility {
    pub actor: Entity,
    pub ability: AbilityId,
    pub target: Entity,
}

/// Emitted by validation after UseAbility passes all checks.
/// Separate type so MessageReader<UseAbility> and MessageWriter don't conflict.
#[derive(Message, Clone)]
pub struct ValidatedAction {
    pub actor: Entity,
    pub ability: AbilityId,
    pub target: Entity,
}

#[derive(Message)]
pub struct ApplyDamage {
    pub source: Entity,
    pub target: Entity,
    pub amount: i32,         // raw, before armor
    pub breakdown: String,   // e.g. "1d8=6 + 4(сил) = 10"
    pub pierces_armor: bool, // true for spells: armor and status bonuses are ignored
}

#[derive(Message)]
pub struct ApplyStatus {
    pub source: Entity, // whose EndTurn ticks this status
    pub target: Entity,
    pub status: StatusId,
    pub duration_rounds: u32,
}

#[derive(Message)]
pub struct ApplyHeal {
    pub source: Entity,
    pub target: Entity,
    pub amount: i32,
    pub breakdown: String, // e.g. "1d4=2 + 1(сила) + 2(инт) = 5"
}

#[derive(Message)]
pub struct EndTurn {
    pub actor: Entity,
}
