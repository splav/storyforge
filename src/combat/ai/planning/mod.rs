//! Turn-level plan generation and simulation.
//!
//! Layout:
//! - `types` — `PlanStep`, `TurnPlan`, `StepOutcome`.
//! - `sim`   — pure simulation of plan steps on a cloned snapshot.

pub mod adaptation;
pub mod generator;
pub mod killable_gate;
pub mod picker;
pub mod reach;
pub mod sanity;
pub mod scorer;
pub mod sim;
pub mod types;

pub use adaptation::{apply_adaptation, Adaptation, AdaptationReason, EvaluationMode};
pub use generator::generate_plans;
pub use killable_gate::{apply_killable_gate, plan_is_offensive_vs, GateStats, KillLineStrength, KILLABLE_ALPHA};
pub use picker::{commit_plan, pick_best_plan, record_committed_reservations, PickMechanics};
pub use reach::reach_from;
pub use sanity::{apply_protect_self_mask, plan_is_defensive, sanity_adjust_plans};
pub use scorer::{
    compute_plan_factors, finalize_scores, rescore_with_intent, rescore_with_per_plan_modes,
    score_plans_with_raw,
};
pub use sim::SimState;
pub use types::{CommittedPrefix, PlanStep, StepOutcome, TurnPlan};
