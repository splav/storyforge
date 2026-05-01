//! Intent layer — tactical-level decision making.
//!
//! Sub-modules:
//! - `kinds`         — TacticalIntent / IntentKind / IntentReason types.
//! - `memory`        — AiMemory + PlanSnapshot (between-tick state).
//! - `select`        — choosing an intent given world facts.
//! - `score`         — numeric evaluation of plan steps under an intent.
//! - `agenda`, `bands`, `considerations` — already-cohesive sub-modules
//!   (untouched by P4).

pub mod agenda;
pub mod bands;
pub mod considerations;
pub mod kinds;
pub mod memory;
pub mod score;
pub mod select;

pub use agenda::{build_agenda, Agenda, AgendaItem};
pub use bands::{assign_band, BandReason, BandWeights, PriorityBand};
pub use considerations::{compute_considerations, IntentConsiderations};
pub use kinds::{IntentKind, IntentReason, TacticalIntent};
pub use memory::{AiMemory, PlanSnapshot, status_hash};
pub use score::{
    cc_reach, evaluate_last_stand_step, intent_score, pursuit_move_score, IntentWeights,
};
#[allow(deprecated)]
pub use select::{
    default_focus_target, intent_viability_threshold, select_intent,
    update_memory, IntentChoice,
};
pub(crate) use select::select_intent_normal;
