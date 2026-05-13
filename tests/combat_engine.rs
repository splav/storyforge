//! Test binary for `crates/combat_engine/` + `src/combat/engine_bridge.rs`.
//!
//! All engine-layer integration tests compile into this single binary so a
//! lib-level change triggers one relink instead of seven.  Module files live
//! under `tests/combat_engine/`.
//!
//! Subset filter: `cargo test --test combat_engine dice::` for one module.

#[path = "combat_engine/bridge_smoke.rs"]
mod bridge_smoke;

#[path = "combat_engine/dice.rs"]
mod dice;

#[path = "combat_engine/effect.rs"]
mod effect;

#[path = "combat_engine/parity.rs"]
mod parity;

#[path = "combat_engine/reaction.rs"]
mod reaction;

#[path = "combat_engine/state.rs"]
mod state;

#[path = "combat_engine/step.rs"]
mod step;
