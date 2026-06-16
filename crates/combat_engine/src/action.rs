//! `Action` enum — coarse player/AI intent passed to `step()`.

use hexx::Hex;

use crate::state::UnitId;
use crate::AbilityId;

/// A high-level combat intent.  The engine validates and expands each variant
/// into a stream of `Effect`s.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Move {
        actor: UnitId,
        path: Vec<Hex>,
    },
    Cast {
        actor: UnitId,
        ability: AbilityId,
        target: UnitId,
        target_pos: Hex,
    },
    /// Signal that the actor is done with their turn (advances the queue and
    /// drives RoundPhase transitions).
    EndTurn {
        actor: UnitId,
    },
}

/// Engine-level error returned by `step()` on illegal or failed actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The actor `UnitId` is not present in the state.
    UnknownActor,
    /// The provided path is empty or otherwise invalid.
    NoPath,
    /// The actor has insufficient movement points for this path.
    OutOfMP,
    /// An effect targeted a unit that died earlier in the same effect queue.
    TargetGone,
    /// Reaction chain exceeded the depth limit (100).
    ReactionDepthExceeded,
    /// An intermediate hex along the path is held by an enemy of the actor.
    PathBlockedByEnemy { hex: Hex },
    /// The destination hex is held by some other unit (friend or foe).
    DestinationOccupied { hex: Hex },
    /// Cast was legally rejected — see `IllegalReason` for the specific cause.
    Illegal(crate::legality::IllegalReason),
}
