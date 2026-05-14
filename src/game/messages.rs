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
    /// Primary target entity. For AoE on empty cell, set to actor.
    pub target: Entity,
    /// Hex position of the target (entity pos or clicked cell for AoE).
    pub target_pos: hexx::Hex,
}

/// Emitted by validation after UseAbility passes all checks.
/// Separate type so MessageReader<UseAbility> and MessageWriter don't conflict.
#[derive(Message, Clone)]
pub struct ValidatedAction {
    pub actor: Entity,
    pub ability: AbilityId,
    pub target: Entity,
    pub target_pos: hexx::Hex,
    /// Target is within max range but below min range — roll twice, take lower.
    pub disadvantage: bool,
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

/// Action input for the engine-backed combat pipeline.
///
/// `process_action_system` in `engine_bridge` reads this message and routes it
/// to `combat_engine::step()`.  The engine is the sole owner of `Action::Move`
/// after Phase 1.
#[derive(Message, Debug)]
pub enum ActionInput {
    Move { actor: Entity, path: Vec<hexx::Hex> },
}

#[derive(Message)]
pub struct EndTurn {
    pub actor: Entity,
}

/// Перезапустить текущий бой: восстановить всех участников, сохранив инициативу.
#[derive(Message)]
pub struct RestartCombat;

/// Эмитируется `resolve_action_system` при использовании способности с
/// `EffectDef::Summon`. Обрабатывается `apply_spawn_system` в `CombatStep::Execute`.
#[derive(Message, Clone)]
pub struct SpawnUnit {
    pub summoner: Entity,
    pub template_id: String,
    pub max_active: Option<u32>,
}
