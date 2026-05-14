//! `CombatState` — canonical in-engine battle state.
//!
//! Uses `Vec<Unit>` for deterministic iteration order (critical for replay)
//! with a `HashMap<UnitId, usize>` index for O(1) lookup. (Decision 6.1.)
//!
//! `UnitId(u64)` is an opaque new-type; the Entity↔UnitId mapping lives in
//! `crate::combat::engine_bridge` (the Bevy boundary). (Decision 6.2.)

use std::collections::HashMap;

use hexx::Hex;

use crate::StatusId;

// ── Identity ──────────────────────────────────────────────────────────────────

/// Opaque unit identifier inside the engine.  Maps 1-to-1 with a Bevy
/// `Entity` via `crate::combat::engine_bridge::UnitIdMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnitId(pub u64);

// ── Resource pools ────────────────────────────────────────────────────────────

/// A (current, max) resource pool that may or may not exist on a unit.
pub type Pool = (i32, i32);

// ── Status effects ────────────────────────────────────────────────────────────

/// Engine-local mirror of `game::components::ActiveStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveStatus {
    pub id: StatusId,
    pub rounds_remaining: u32,
    /// DoT damage per end-of-turn tick. 0 = no DoT.
    pub dot_per_tick: i32,
    /// The unit whose end-turn ticks down `rounds_remaining`.  Used by
    /// status-removal (`advance_turn`) and aura cleanup (`auras_system`) to
    /// distinguish ability-applied from aura-applied entries.  Engine itself
    /// doesn't read this in Phase 2 — recorded for the projector + Phase 3
    /// DoT tick attribution.
    pub applier: UnitId,
}

// ── Team ──────────────────────────────────────────────────────────────────────

/// Combat team — canonical engine-side enum.  `game::components::Team`
/// is a re-export of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Team {
    Player,
    Enemy,
}

// ── Round phase ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundPhase {
    PreRound,
    ActorTurn,
    EndRound,
}

// ── Unit ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Unit {
    pub id: UnitId,
    pub team: Team,
    pub pos: Hex,
    /// Current HP. 0 = dead (unit stays in `units` vec as tombstone).
    pub hp: i32,
    pub max_hp: i32,
    /// Base armor value (equipment). Bonus from statuses is tracked separately
    /// and folded in by `refresh_aggregates`.
    pub armor: i32,
    /// Armor bonus from active statuses (recomputed by `RefreshAggregates`).
    pub armor_bonus: i32,
    /// Base speed (without status speed_bonus).
    pub base_speed: i32,
    /// Effective speed = base_speed + speed bonuses from statuses.
    pub speed: i32,
    pub action_points: i32,
    pub movement_points: i32,
    pub reactions_left: i32,
    pub statuses: Vec<ActiveStatus>,
    /// `None` if the unit has no rage mechanic.
    pub rage: Option<Pool>,
    /// `None` if the unit has no mana mechanic.
    pub mana: Option<Pool>,
    /// `None` if the unit has no energy mechanic.
    pub energy: Option<Pool>,
}

impl Unit {
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }
}

// ── CombatState ───────────────────────────────────────────────────────────────

/// Canonical engine state for one combat encounter.
///
/// `units` is the authoritative list; `idx` is a derived cache — always in
/// sync via `insert_unit` / `remove_unit`.  Never mutate `units` directly;
/// go through the provided methods so the cache stays consistent.
#[derive(Debug, Clone)]
pub struct CombatState {
    units: Vec<Unit>,
    /// `UnitId → index` in `units`. Rebuilt by `rebuild_idx` after bulk mutations.
    idx: HashMap<UnitId, usize>,
    pub round: u32,
    pub phase: RoundPhase,
    /// Seed carried along for replay reproducibility.
    pub random_seed: u64,
}

impl Default for CombatState {
    fn default() -> Self {
        Self::new(vec![], 0, RoundPhase::PreRound, 0)
    }
}

impl CombatState {
    /// Construct from a pre-built unit list. Eagerly builds the index.
    pub fn new(units: Vec<Unit>, round: u32, phase: RoundPhase, random_seed: u64) -> Self {
        let mut state = Self {
            units,
            idx: HashMap::new(),
            round,
            phase,
            random_seed,
        };
        state.rebuild_idx();
        state
    }

    /// Rebuild the `UnitId → index` cache after any bulk mutation.
    pub fn rebuild_idx(&mut self) {
        self.idx.clear();
        for (i, u) in self.units.iter().enumerate() {
            self.idx.insert(u.id, i);
        }
    }

    /// Look up a unit by id. Returns `None` if not present.
    pub fn unit(&self, id: UnitId) -> Option<&Unit> {
        self.idx.get(&id).map(|&i| &self.units[i])
    }

    /// Mutable unit lookup.
    pub fn unit_mut(&mut self, id: UnitId) -> Option<&mut Unit> {
        self.idx.get(&id).map(|&i| &mut self.units[i])
    }

    /// Iterate all units (alive + dead tombstones).
    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    /// Iterate alive units only.
    pub fn alive_units(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter().filter(|u| u.is_alive())
    }

    /// All living enemies of `actor_id`.
    pub fn enemies_of(&self, actor_id: UnitId) -> impl Iterator<Item = &Unit> {
        let team = self.unit(actor_id).map(|u| u.team);
        self.units.iter().filter(move |u| u.is_alive() && Some(u.team) != team)
    }
}
