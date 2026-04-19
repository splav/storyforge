//! Turn-level plan generation and simulation.
//!
//! Layout:
//! - `types` — `PlanStep`, `TurnPlan`, `StepOutcome`.
//! - `sim`   — pure simulation of plan steps on a cloned snapshot.

pub mod generator;
pub mod picker;
pub mod sanity;
pub mod scorer;
pub mod sim;
pub mod types;

pub use generator::generate_plans;
pub use picker::{
    decision_from_plan, decision_from_steps, pick_best_plan, plan_to_candidate,
    record_plan_reservation, steps_consumed_by_decision, validate_plan_step,
};
pub use sanity::{apply_protect_self_mask, plan_is_defensive, sanity_adjust_plans};
pub use scorer::{compute_plan_factors, score_plans, score_plans_with_raw};
pub use sim::SimState;
pub use types::{PlanStep, StepOutcome, TurnPlan};
