//! Rule 2: final tile has fewer than 2 open neighbours (retreat trap).
//!
//! A tile with < 2 open (in-bounds, unoccupied by allies) neighbours is
//! considered a trap — the unit has no room to manoeuvre next turn.

use crate::combat::ai::pipeline::stages::sanity::{SanityHit, SanityRule};
use crate::game::hex::{in_bounds, Hex};
use std::collections::HashSet;

/// Evaluate the RetreatTrap rule for one plan.
///
/// Returns `Some(SanityHit)` if the final destination has fewer than 2 open
/// neighbours, `None` otherwise.
pub(super) fn evaluate(final_pos: Hex, ally_positions: &HashSet<Hex>) -> Option<SanityHit> {
    let open_neighbors = final_pos
        .all_neighbors()
        .iter()
        .filter(|&&n| in_bounds(n) && !ally_positions.contains(&n))
        .count();
    if open_neighbors < 2 {
        Some(SanityHit {
            rule: SanityRule::RetreatTrap,
            multiplier: 0.5,
        })
    } else {
        None
    }
}
