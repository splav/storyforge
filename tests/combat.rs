//! Test binary for `src/combat/` (legacy Bevy pipeline) + AI sim + replay.
//!
//! All combat-pipeline integration tests compile into this single binary so a
//! lib-level change triggers one relink instead of twelve.  Module files live
//! under `tests/combat/`.
//!
//! Subset filter: `cargo test --test combat aoo::` for one module.
//!
//! This binary's surface shrinks across Phase 1-6 as `movement_system`,
//! `apply_effects_system`, `status_apply_system` etc. get deleted in favor of
//! the engine.  Surviving tests migrate to `tests/combat_engine.rs`.

#[path = "common/mod.rs"]
mod common;

#[path = "combat/ai_scenarios.rs"]
mod ai_scenarios;

#[path = "combat/aoo.rs"]
mod aoo;

#[path = "combat/auras.rs"]
mod auras;

#[path = "combat/crit_fail.rs"]
mod crit_fail;

#[path = "combat/effects.rs"]
mod effects;

#[path = "combat/equipment.rs"]
mod equipment;

#[path = "combat/movement.rs"]
mod movement;

#[path = "combat/pipeline.rs"]
mod pipeline;

#[path = "combat/replay_assert.rs"]
mod replay_assert;

#[path = "combat/sim_parity.rs"]
mod sim_parity;

#[path = "combat/statuses.rs"]
mod statuses;

#[path = "combat/validation.rs"]
mod validation;
