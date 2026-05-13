//! `ContentView` — read-only content access trait for the engine.
//!
//! The engine needs only a minimal slice of `ContentDb`.  This trait expresses
//! exactly that slice so the engine has zero dependency on `crate::content`.
//!
//! **Phase 0** exposes only what `step(Action::Move)` needs:
//! - `aoo_dice(attacker)` — weapon dice for AoO expansion.
//! - `status_bonuses(id)` — speed/armor bonuses for `RefreshAggregates`.
//!
//! Callers implement this trait for real (`ActiveContent` adapter); the engine
//! only ever calls through the trait object.  Step 8+ agent extends as needed.

use crate::{dice::DiceExpr, state::UnitId, StatusId};

/// Per-status stat bonuses relevant to engine aggregate recomputation.
///
/// Mirrors the fields read by `BattleSnapshot::refresh_aggregates` and
/// `snapshot::status_bonuses`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusBonuses {
    /// Added to `base_speed` to get effective `speed`.
    pub speed_bonus: i32,
    /// Added to equipment `armor` to get effective mitigation.
    pub armor_bonus: i32,
}

/// Read-only view onto game content that the engine needs.
///
/// Implemented by `crate::combat::engine_bridge::BridgeContent` (which holds
/// a reference to `ActiveContent`).  Test implementations return simple stubs.
pub trait ContentView {
    /// Weapon dice for the attacker's AoO strike.
    ///
    /// Returns `None` if the unit has no equipped melee weapon — in which case
    /// `expand_reaction` will not emit an AoO (mirroring `movement_system`'s
    /// "prefer weapon dice; if no weapon, skip" logic).
    fn aoo_dice(&self, attacker: UnitId) -> Option<DiceExpr>;

    /// Stat bonuses granted by a single status instance.
    ///
    /// Returns `StatusBonuses::default()` (all zeros) for unknown status ids.
    /// This matches the real path: statuses not present in `content.statuses`
    /// contribute nothing.
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses;
}
