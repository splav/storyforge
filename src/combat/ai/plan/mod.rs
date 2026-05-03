//! Turn-level plan generation and simulation.
//!
//! Layout:
//! - `types` — `PlanStep`, `TurnPlan`, `StepOutcome`.
//! - `sim`   — pure simulation of plan steps on a cloned snapshot.
//! - `reach` — reachability helpers.
//! - `generator` — plan enumeration.
//! - `future_value` — multi-step lookahead scoring.
//!
//! Scoring and terminal-state aggregation have been extracted:
//! - Factor aggregation (scorer) → `crate::combat::ai::scoring::factors::aggregate`.
//! - Terminal state evaluation   → `crate::combat::ai::scoring::factors::terminal_state`.

pub mod future_value;
pub mod generator;
pub mod reach;
pub mod sim;
pub mod types;
#[cfg(test)]
mod parity_tests;

pub use generator::generate_plans;
pub use reach::reach_from;
pub use sim::SimState;
pub use types::{CommittedPrefix, PlanStep, StepOutcome, TurnPlan};

// Re-export scoring helpers so existing callers of `crate::combat::ai::plan::*`
// still work without churn. These now live in scoring/factors/aggregate.
pub use crate::combat::ai::scoring::factors::aggregate::{
    build_summon_dpr_cache, compute_plan_factors, compute_plan_factors_sans_intent,
    compute_plan_intent_sum, factor_contribution, aggregate_factors_to_score, rescore_with_intent,
    rescore_with_per_plan_modes, score_plans_with_raw, worst_path_danger,
};
