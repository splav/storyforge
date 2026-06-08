use crate::state::UnitId;

/// Engine-side turn order queue.
///
/// Mirrors the semantics of `src/game/resources.rs::TurnQueue` but operates on
/// `UnitId` instead of Bevy `Entity`.  The ECS `Res<TurnQueue>` is kept as a
/// one-way projection (engine → ECS) for UI consumers — see D1 in Phase 4 plan.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_queue(len: usize, index: usize) -> TurnQueue {
        TurnQueue {
            order: (0..len as u64).map(UnitId).collect(),
            index,
        }
    }

    /// Simulates the advance-and-skip logic (mirrors `Effect::AdvanceTurn` predicate).
    /// Returns `(wrapped, final_index)`.
    fn sim_advance(queue: &mut TurnQueue, is_alive: impl Fn(usize) -> bool) -> (bool, usize) {
        let start_idx = queue.index;
        let mut wrapped = false;

        let prev = queue.index;
        queue.advance();
        if queue.wrapped_after(prev) {
            wrapped = true;
        }

        loop {
            if is_alive(queue.index) {
                break;
            }
            let prev = queue.index;
            queue.advance();
            if queue.wrapped_after(prev) {
                wrapped = true;
            }
            if queue.index == start_idx {
                break;
            }
        }

        (wrapped, queue.index)
    }

    /// Normal mid-round handoff: index 1 → 2 (alive), no wrap.
    #[test]
    fn advance_mid_round_no_wrap() {
        let mut q = make_queue(3, 1);
        let (wrapped, next) = sim_advance(&mut q, |idx| idx == 2);
        assert!(!wrapped);
        assert_eq!(next, 2);
    }

    /// End-of-round: last slot (2) → wrap to 0 (alive) → StartRound.
    #[test]
    fn advance_end_of_round_wraps() {
        let mut q = make_queue(3, 2);
        let (wrapped, _) = sim_advance(&mut q, |idx| idx == 0);
        assert!(wrapped);
    }

    /// Dead unit at index 0 (Morok summoned, dies each round):
    /// slot 2 → wrap to 0 (dead, wrap detected!) → skip → 1 (alive).
    #[test]
    fn advance_dead_at_zero_after_wrap_still_detects_wrap() {
        let mut q = make_queue(3, 2);
        let (wrapped, next) = sim_advance(&mut q, |idx| idx == 1);
        assert!(wrapped, "wrap must be detected even when slot 0 is dead");
        assert_eq!(next, 1);
    }

    /// All dead: loop exhausts queue back to start_idx, wrapped=true.
    #[test]
    fn advance_all_dead_wraps_back_to_start() {
        let mut q = make_queue(3, 1);
        let (wrapped, _) = sim_advance(&mut q, |_| false);
        assert!(wrapped);
    }

    /// Single-actor queue always wraps.
    #[test]
    fn single_actor_queue_always_wraps() {
        let mut q = make_queue(1, 0);
        let prev = q.index;
        q.advance();
        assert!(q.wrapped_after(prev));
    }

    /// `wrapped_after` returns false for normal mid-round advance.
    #[test]
    fn wrapped_after_false_for_mid_round() {
        let mut q = make_queue(4, 1);
        let prev = q.index;
        q.advance();
        assert!(!q.wrapped_after(prev));
    }
}
