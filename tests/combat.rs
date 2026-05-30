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


#[path = "combat/handoff.rs"]
mod handoff;

#[path = "combat/equipment.rs"]
mod equipment;

#[path = "combat/movement.rs"]
mod movement;

#[path = "combat/replay_assert.rs"]
mod replay_assert;

#[path = "combat/ai_snapshot.rs"]
mod ai_snapshot;

