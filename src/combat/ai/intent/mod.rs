//! Intent layer — tactical-level decision making.
//!
//! Sub-modules:
//! - `kinds`         — TacticalIntent / IntentKind / IntentReason types.
//! - `select`        — choosing an intent given world facts.
//! - `score`         — numeric evaluation of plan steps under an intent.
//! - `agenda`, `bands`, `considerations` — sub-modules for those concerns.
//!
//! `AiMemory` + `PlanSnapshot` live in `memory/ai_memory.rs`; re-exported here
//! for backward-compat.

pub mod agenda;
pub mod bands;
pub mod considerations;
pub mod kinds;
pub mod score;
pub mod select;

pub use agenda::{build_agenda, Agenda, AgendaItem};
pub use bands::{assign_band, BandReason, BandWeights, PriorityBand};
pub use considerations::{compute_considerations, IntentConsiderations};
pub use kinds::{IntentKind, IntentReason, TacticalIntent};
// AiMemory + PlanSnapshot moved to memory/; re-exported for backward-compat.
pub use crate::combat::ai::memory::{status_hash_engine, AiMemory, PlanSnapshot};
pub use score::{
    cc_reach, evaluate_last_stand_step, intent_score, pursuit_move_score, IntentWeights,
};
pub(crate) use select::select_intent_normal;
#[allow(deprecated)]
pub use select::{
    default_focus_target, intent_viability_threshold, select_intent, update_memory, IntentChoice,
};
