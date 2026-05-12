//! # `combat_engine` — pure-Rust combat state machine
//!
//! See `docs/ai/rework/unisim.md` §2.1–2.6 for architecture rationale.
//!
//! **Zero `bevy::` imports anywhere in this tree.**  All Bevy glue lives in
//! `crate::combat::engine_bridge`.
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

pub mod action;
pub mod content;
pub mod dice;
pub mod effect;
pub mod event;
pub mod reaction;
pub mod state;
pub mod step;
