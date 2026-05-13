//! `Action` enum — coarse player/AI intent passed to `step()`.

use hexx::Hex;

use crate::state::UnitId;

/// A high-level combat intent.  The engine validates and expands each variant
/// into a stream of `Effect`s.
///
/// Phase 0 implements only `Move`; other variants are stubs present so the
/// type system is complete.
#[derive(Debug, Clone)]
pub enum Action {
    Move { actor: UnitId, path: Vec<Hex> },
    // Future variants (Phase 1+):
    // Cast  { actor: UnitId, ability: AbilityId, target: ActionTarget },
    // EndTurn { actor: UnitId },
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
}
