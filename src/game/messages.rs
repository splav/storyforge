//! Combat messages.
//!
//! Input → UseAbility → validation → resolution → ApplyDamage/ApplyStatus → EndTurn
//! Each step is a separate system reacting to these messages.

use bevy::prelude::*;
use crate::core::{AbilityId, StatusId};

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
    pub amount: i32,
}

#[derive(Message)]
pub struct ApplyStatus {
    pub target: Entity,
    pub status: StatusId,
    pub duration_rounds: u32,
}

#[derive(Message)]
pub struct EndTurn {
    pub actor: Entity,
}
