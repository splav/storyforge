//! Test binary for `src/combat/` (legacy Bevy pipeline) + AI sim + replay.
//!
//! All combat-pipeline integration tests share this binary so a lib change
//! relinks once, not twelve times. Module files live under `tests/combat/`.
//! Subset filter: `cargo test --test combat aoo::` for one module.
//!
//! Surface shrinks as legacy systems migrate to the engine; surviving tests
//! move to `tests/combat_engine.rs`.

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

#[path = "combat/ai_no_abilities.rs"]
mod ai_no_abilities;

#[path = "combat/mana_gear.rs"]
mod mana_gear;
