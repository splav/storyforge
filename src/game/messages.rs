//! Combat messages.
//!
//! Input → ActionInput::Cast → process_action_system → engine → project_state → EndTurn
//! Each step is a separate system reacting to these messages.

use bevy::prelude::*;

#[derive(Message)]
pub struct StartCombat {
    pub encounter: Entity,
}

/// Action input for the engine-backed combat pipeline.
///
/// `process_action_system` in `engine_bridge` reads this message and routes it
/// to `combat_engine::step()`.  The engine is the sole authority for both
/// `Action::Move` (since Phase 1) and `Action::Cast` (since Phase 2 step 9d).
#[derive(Message, Debug)]
pub enum ActionInput {
    Move { actor: Entity, path: Vec<hexx::Hex> },
    Cast {
        actor: Entity,
        ability: crate::core::AbilityId,
        target: Entity,
        target_pos: hexx::Hex,
    },
}

#[derive(Message)]
pub struct EndTurn {
    pub actor: Entity,
}

/// Перезапустить текущий бой: восстановить всех участников, сохранив инициативу.
#[derive(Message)]
pub struct RestartCombat;

/// Эмитируется `process_action_system` при обнаружении способности с
/// `EffectDef::Summon`. Обрабатывается `apply_spawn_system` в `CombatStep::Execute`.
#[derive(Message, Clone)]
pub struct SpawnUnit {
    pub summoner: Entity,
    pub template_id: String,
    pub max_active: Option<u32>,
}
