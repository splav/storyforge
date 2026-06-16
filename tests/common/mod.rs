//! Shared scaffolding for integration tests (`tests/combat/` and
//! `tests/combat_engine/`).
//!
//! ```text
//! tests/common/
//!   mod.rs              — declarations + flat-path re-exports (this file)
//!   fixtures.rs         — base_stats, test_equipment, hero/enemy bundles, message helpers
//!   apps/
//!     engine.rs         — movement_app, init_engine_state
//!     bridge.rs         — bridge_app, projector_only_app, spawn_*, MeleeContent, etc.
//!   scenarios/
//!     statuses.rs       — insert_stun_status (and future insert_*_status helpers)
//! ```

#![allow(dead_code)]

pub mod apps;
pub mod bin;
pub mod engine_unit;
pub mod fixtures;
pub mod scenarios;
