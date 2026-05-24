//! App builders per layer.
//!
//! - `engine` — `movement_app` (full state machine + bridge schedule).
//! - `bridge` — `bridge_app` (no state machine, manual `bootstrap`).

pub mod bridge;
pub mod engine;
