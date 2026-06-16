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
/// `process_action_system` in `bridge` reads this and routes it to
/// `combat_engine::step()` — the sole authority for Move, Cast, and EndTurn.
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
