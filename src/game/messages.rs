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
/// `Action::EndTurn` routes here since Phase 4 step 4e.
#[derive(Message, Debug)]
pub enum ActionInput {
    Move {
        actor: Entity,
        path: Vec<hexx::Hex>,
    },
    Cast {
        actor: Entity,
        ability: combat_engine::AbilityId,
        target: Entity,
        target_pos: hexx::Hex,
    },
    EndTurn {
        actor: Entity,
    },
}

/// Перезапустить текущий бой: восстановить всех участников, сохранив инициативу.
#[derive(Message)]
pub struct RestartCombat;
