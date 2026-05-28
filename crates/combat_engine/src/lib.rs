//! # `combat_engine` — pure-Rust combat state machine
//!
//! See `docs/ai/rework/unisim.md` §2.1–2.6 for architecture rationale.
//!
//! **Zero `bevy::` imports anywhere in this tree.**  All Bevy glue lives in
//! `storyforge::combat::engine_bridge`.
//!
//! ## Concept hierarchy
//! - **Action** — coarse player/AI intent (`Move`, `Cast`, …).
//! - **Effect** — atomic state mutation produced by the engine.
//! - **Event** — observable fact emitted for UI/log/replay consumers.
//!
//! ## Entry point
//! ```ignore
//! let events = combat_engine::step(&mut state, action, &mut rng, &content)?;
//! ```

// ── StatusId ──────────────────────────────────────────────────────────────────

/// Macro to create a newtype string id with standard trait impls.
/// Mirrors `storyforge::core::string_id!` — kept in sync manually.
macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                $name(s)
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

string_id!(AbilityId);
string_id!(ArmorId);
string_id!(StatusId);
string_id!(WeaponId);

// ── ResourceKind ──────────────────────────────────────────────────────────────

/// Вид ресурса, который может тратиться на способности.
///
/// Mirrors `storyforge::core::ResourceKind` — kept in sync manually.
/// `storyforge::core` re-exports this type directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ResourceKind {
    Hp,
    Mana,
    Rage,
    Energy,
}

// ── Phase C pool infrastructure ───────────────────────────────────────────────

/// Six spendable / regenerable resource pools per unit.
///
/// **Iteration order is load-bearing.** Determinism contract: replay-trace
/// hashing depends on `enum_map::Iter` order, which follows variant
/// declaration order. Adding a variant in the middle is a SCHEMA bump.
///
/// `Hp` is the first variant (HP-as-pool migration, completed Stage 3c).
/// `pools[Hp]` is the **canonical source of truth** for HP — legacy
/// `Unit.hp` / `Unit.max_hp` fields were removed in Stage 3c (v44).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, enum_map::Enum, serde::Serialize, serde::Deserialize)]
pub enum PoolKind {
    Hp,       // Canonical HP pool since Stage 3c (v44); legacy fields removed.
    Mana,
    Rage,
    Energy,
    Ap,
    Mp,
}

/// Per-pool turn-start regeneration policy. Stored on `UnitTemplate`.
/// Used by `state.rs::start_actor_turn` to drive the unified regen loop.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum RegenRule {
    /// No turn-start change. Used by Rage (only gains via combat).
    #[default]
    None,
    /// Add `amount`, clamp at max. Used by Mana, Energy.
    Increment(i32),
    /// Set current = max unconditionally. Used by Ap, Mp.
    RefillToMax,
}

/// Reason a pool's current/max changed. Carried on `Event::PoolChanged`
/// (added in C4). Bridge mirror parameterizes log/UI rendering by this.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PoolChangeCause {
    /// Turn-start regen step (Increment rule fired).
    Regen,
    /// Turn-start refill (RefillToMax rule fired, current changed to max).
    Refill,
    /// Resource was spent — PayCost / DecrementAP / DecrementMP.
    Spent,
    /// Resource was gained outside regen (e.g. GainRage on damage).
    Gained,
    /// RefreshAggregates updated the `max` (e.g. speed_bonus from a status
    /// changed MP-max, status-derived AP-max change).
    MaxChanged,
}

/// Модификатор характеристики: floor(stat / 2).
/// Диапазон характеристик −5..10 → модификаторы −3..+5.
pub fn modifier(stat: i32) -> i32 {
    stat >> 1 // арифметический сдвиг = floor для степеней двойки
}

/// Sentinel value for status durations that never expire.
///
/// Used for `initial_statuses` applied at bootstrap (e.g. permanent stun on
/// non-acting party NPCs). `ExpireStatus` guards against this value and skips
/// the decrement, so the status persists for the entire combat.
pub const PERMANENT_DURATION: u32 = u32::MAX;

/// Re-exported so crates that depend on `combat_engine` can use the `enum_map!`
/// macro without declaring their own `enum-map` dependency.
pub use enum_map;

pub mod action;
pub mod geom;
pub mod toml_content_view;
pub mod content;
pub mod content_hash;
pub mod dice;
pub mod effect;
pub mod event;
pub mod legality;
pub mod reaction;
pub mod state;
pub mod step;
pub mod targeting;
pub mod trace;
pub mod turn_queue;

pub use dice::{DiceExpr, DiceRng};
pub use geom::has_los;
pub use content::{AbilityDef, AbilityRange, AoEShape, AuraDef, AuraEffects, CasterContext, Cost, CritFailOutcome, EffectDef, PhaseEntry, PhaseTransition, StatusApplication, StatusBonuses, StatusDef, StatusOn, TargetType, TeamRelation, UnitTemplate};
pub use effect::{final_damage_f32, SpawnBlockedReason};
pub use targeting::aoe_cells;
pub use toml_content_view::{TomlContentView, LoadError};
pub use legality::{check_legality, ActionState, ActorView, IllegalReason, LegalAction, ProposedAction};
pub use step::EngineCheckState;
pub use turn_queue::TurnQueue;
