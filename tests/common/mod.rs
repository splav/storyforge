//! Shared scaffolding for integration tests (`tests/combat/` and
//! `tests/combat_engine/`).
//!
//! Restructured in Phase H3 of `docs/refactor/helpers-normalization-plan.md`.
//! Layout:
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
//!
//! ## Compatibility re-exports
//!
//! Existing tests use `common::base_stats()`, `common::bridge::bridge_app()`,
//! `common::movement_app()` — these paths are preserved via the re-exports below.

#![allow(dead_code)]

pub mod engine_unit;
pub mod fixtures;
pub mod apps;
pub mod scenarios;
