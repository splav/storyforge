use crate::state::UnitId;

/// Engine-side turn order queue.
///
/// Mirrors the semantics of `src/game/resources.rs::TurnQueue` but operates on
/// `UnitId` instead of Bevy `Entity`.  The ECS `Res<TurnQueue>` is kept as a
/// one-way projection (engine → ECS) for UI consumers — see D1 in Phase 4 plan.
#[derive(Debug, Clone, Default)]
pub struct TurnQueue {
    pub order: Vec<UnitId>,
    pub index: usize,
}

impl TurnQueue {
    pub fn new(order: Vec<UnitId>) -> Self {
        Self { order, index: 0 }
    }

    /// Returns the `UnitId` of the currently active actor, or `None` if the
    /// queue is empty.
    pub fn current(&self) -> Option<UnitId> {
        self.order.get(self.index).copied()
    }

    /// Advance the cursor by one position, wrapping around modulo queue length.
    /// On an empty queue the index stays at 0.
    pub fn advance(&mut self) {
        self.index = (self.index + 1) % self.order.len().max(1);
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Returns `true` if the cursor has wrapped around past `prev_idx`.
    ///
    /// Wrap signal: `self.index < prev_idx` (matches the ECS modulo behaviour).
    /// Special case: a length-1 queue always "wraps to itself" on every advance
    /// (`index` stays at 0).  The convention adopted here is that this counts as
    /// a wrap so that `BumpRound` fires every turn for a singleton queue —
    /// otherwise a single actor could loop forever without incrementing the round.
    pub fn wrapped_after(&self, prev_idx: usize) -> bool {
        if self.order.len() == 1 {
            // Single-actor queue: every advance is a wrap-to-self; treat as wrap.
            return true;
        }
        self.index < prev_idx || self.order.is_empty()
    }
}
