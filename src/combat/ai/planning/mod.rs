//! Turn-level plan generation and simulation.
//!
//! Layout:
//! - `types` — `PlanStep`, `TurnPlan`, `StepOutcome`.
//! - `sim`   — pure simulation of plan steps on a cloned snapshot.

pub mod generator;
pub mod picker;
pub mod reach;
pub mod sanity;
pub mod scorer;
pub mod sim;
pub mod types;

pub use generator::generate_plans;
pub use picker::{
    commit_plan, pick_best_plan, record_committed_reservations, PickMechanics,
};
pub use reach::reach_from;
pub use sanity::{apply_protect_self_mask, plan_is_defensive, sanity_adjust_plans};
pub use scorer::{compute_plan_factors, rescore_with_intent, score_plans_with_raw};
pub use sim::SimState;
pub use types::{CommittedPrefix, PlanStep, StepOutcome, TurnPlan};
