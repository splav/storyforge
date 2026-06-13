//! Bevy ↔ `combat_engine` boundary.
//!
//! This module is the **only** place that imports both `bevy::` and
//! `combat_engine::`.  The engine itself (`crates/combat_engine/`) has zero
//! Bevy dependency (decision 6.7).
//!
//! # Submodule map
//!
//! | Module | Contents |
//! |---|---|
//! | [`ids`] | `UnitIdMap` resource, `entity_to_uid` encoding |
//! | [`bootstrap`] | `from_ecs`, `build_unit`, `bootstrap_combat_state` (ECS → engine init) |
//! | [`content_view`] | `EcsContentView` — engine `ActiveContentData` backed by `ActiveContent` |
//! | [`translate`] | `translate_one` — exhaustive engine `Event` → CombatLog/queues match |
//! | [`process`] | `process_action_system` (ActionInput → `step()`), dynamic summon spawn |
//! | [`project`] | `project_state_to_ecs` — engine state → ECS read-only projection |
//! | [`queues`] | `BridgeQueues` + pre/post-projection drain systems, mirror resets |
//! | [`phases`] | boss-phase ECS writes + victory-override/deadline application |
//!
//! All items are re-exported flat: callers use `crate::combat::bridge::X`
//! without naming the submodule.
//!
//! ## `Entity → UnitId` encoding
//!
//! Uses `Entity::to_bits()` — Bevy's own canonical u64 serialization of an
//! entity (low bits = index, high bits = generation).  Stable within a session;
//! not stable across save/load (generation counters reset).

use bevy::prelude::*;
use combat_engine::state::CombatState;

pub mod bootstrap;
pub mod content_view;
pub mod ids;
pub mod phases;
pub mod process;
pub mod project;
pub mod queues;
pub mod translate;

pub use bootstrap::*;
pub use content_view::*;
pub use ids::*;
pub use phases::*;
pub use process::*;
pub use project::*;
pub use queues::*;
pub(crate) use translate::*;

// ── CombatStateRes ────────────────────────────────────────────────────────────

/// Bevy resource wrapper for the pure `CombatState`.
///
/// Exists solely so `CombatState` (which lives in `combat_engine/` with zero
/// Bevy imports) can be stored as a Bevy `Res`.  Initialized once per combat
/// by `bootstrap_combat_state`; engine state is authoritative from combat
/// start, ECS mirrors via `project_state_to_ecs`.
#[derive(Resource, Default)]
pub struct CombatStateRes(pub CombatState);
