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

use crate::{dice::DiceExpr, state::UnitId, AbilityId, ResourceKind, StatusId};

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

/// Resource cost for an ability (one entry per resource kind).
///
/// Mirrors `crate::content::abilities::ResourceCost`.
#[derive(Debug, Clone, Copy)]
pub struct Cost {
    pub resource: ResourceKind,
    pub amount: i32,
}

/// Who/where an ability targets.  Mirrors `crate::content::abilities::TargetType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    SingleAlly,
    Myself,
    Ground,
}

/// Range in hex-steps.  `max == 0` means self-only.
/// Mirrors `crate::content::abilities::AbilityRange`.
#[derive(Debug, Clone, Copy)]
pub struct AbilityRange {
    pub min: u32,
    pub max: u32,
}

/// Engine-side minimal ability definition — legality-relevant fields only.
///
/// Phase 2 step 5 will extend with `aoe: AoEShape` (targeting); step 6 will
/// add `effect: EffectDef` + `statuses: Vec<StatusApplication>` when
/// `Action::Cast` lands in `step()`.
#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub key: Option<String>,
    pub cost_ap: i32,
    pub costs: Vec<Cost>,
    pub range: AbilityRange,
    pub target_type: TargetType,
}

/// Engine-side minimal status definition — legality + aggregate-relevant fields.
///
/// Aggregates (`armor_bonus`, `speed_bonus`) overlap with `StatusBonuses`;
/// Phase 2 keeps both, `status_bonuses()` may later be derived from this.
#[derive(Debug, Clone, Copy)]
pub struct StatusDef {
    pub causes_disadvantage: bool,
    pub blocks_mana_abilities: bool,
    pub forces_targeting: bool,
    pub skips_turn: bool,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    pub speed_bonus: i32,
}

/// Read-only view onto game content that the engine needs.
///
/// Implemented by `crate::combat::engine_bridge::EcsContentView` (live path)
/// and `crate::combat::ai::plan::sim::SnapshotContentView` (sim path).
/// Test implementations return simple stubs.
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

    /// Engine-side ability definition.  `None` if the id is unknown.
    ///
    /// Used by `check_legality` (Phase 2 step 2c) and `expand_action(Cast)`
    /// (Phase 2 step 6).
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef>;

    /// Engine-side status definition.  `None` if the id is unknown.
    fn status_def(&self, id: &StatusId) -> Option<StatusDef>;
}
