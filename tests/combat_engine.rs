//! Test binary for `crates/combat_engine/` + `src/combat/engine_bridge.rs`.
//!
//! All engine-layer integration tests compile into this single binary so a
//! lib-level change triggers one relink instead of seven.  Module files live
//! under `tests/combat_engine/`.
//!
//! Subset filter: `cargo test --test combat_engine dice::` for one module.

#[path = "../tests/common/mod.rs"]
mod common;

#[path = "combat_engine/bridge_cast.rs"]
mod bridge_cast;

#[path = "combat_engine/bridge_movement.rs"]
mod bridge_movement;

#[path = "combat_engine/bridge_phase.rs"]
mod bridge_phase;

#[path = "combat_engine/bridge_projector.rs"]
mod bridge_projector;

#[path = "combat_engine/bridge_trace.rs"]
mod bridge_trace;

#[path = "combat_engine/turn_queue.rs"]
mod turn_queue;

#[path = "combat_engine/cast.rs"]
mod cast;

#[path = "combat_engine/trap.rs"]
mod trap;

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

#[path = "combat_engine/targeting.rs"]
mod targeting;

#[path = "combat_engine/step.rs"]
mod step;

#[path = "combat_engine/end_turn.rs"]
mod end_turn;

#[path = "combat_engine/aura.rs"]
mod aura;

#[path = "combat_engine/phase.rs"]
mod phase;

#[path = "combat_engine/legality_parity.rs"]
mod legality_parity;

#[path = "combat_engine/determinism.rs"]
mod determinism;

#[path = "combat_engine/purity.rs"] mod purity;
#[path = "combat_engine/rng_count.rs"] mod rng_count;
#[path = "combat_engine/aura_determinism.rs"] mod aura_determinism;
#[path = "combat_engine/serde_roundtrip.rs"] mod serde_roundtrip;
#[path = "combat_engine/replay.rs"] mod replay;
#[path = "combat_engine/trace_helpers.rs"] mod trace_helpers;
#[path = "combat_engine/initiative.rs"] mod initiative;
#[path = "combat_engine/hot.rs"] mod hot;
#[path = "combat_engine/tags.rs"] mod tags;
#[path = "combat_engine/phase_tags.rs"] mod phase_tags;
