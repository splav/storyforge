//! Geometry helpers used by the engine and re-exported to storyforge.
//!
//! `has_los` is the single authoritative implementation of the line-of-sight
//! check.  All three `ActionState` backends (`BevyActions`, `SnapshotActionState`,
//! `EngineCheckState`) delegate here so the parity contract is structural, not
//! coincidental.

use hexx::Hex;

/// Returns `true` if the direct hex-line from `from` to `to` is **not** blocked.
///
/// `blocks_los(hex)` should return `true` for cells that block vision.
/// Only intermediate cells are tested — `from` and `to` themselves are excluded.
///
/// # Edge cases
/// - `from == to` → always `true` (self-LOS is unobstructed).
/// - Adjacent cells — no intermediates → always `true`.
pub fn has_los(from: Hex, to: Hex, blocks_los: impl Fn(Hex) -> bool) -> bool {
    if from == to {
        return true;
    }
    let cells: Vec<Hex> = from.line_to(to).collect();
    // Skip first (from) and last (to); check only intermediate hexes.
    cells[1..cells.len() - 1].iter().all(|&h| !blocks_los(h))
}
