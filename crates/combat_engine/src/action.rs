//! `Action` enum — coarse player/AI intent passed to `step()`.

use hexx::Hex;

use crate::state::UnitId;
use crate::AbilityId;

/// A high-level combat intent.  The engine validates and expands each variant
/// into a stream of `Effect`s.
///
/// Phase 0 implements only `Move`; `Cast` is added in Phase 2 step 6b.
/// `EndTurn` is a Phase 3 placeholder — Phase 4 will extend it with queue
/// advance and RoundPhase transitions.
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
    /// Signal that the actor is done with their turn.  Phase 3 arm is a no-op;
    /// Phase 4 will add queue advance + RoundPhase transitions.
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
