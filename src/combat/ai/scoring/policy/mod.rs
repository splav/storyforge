//! Policy formulas — value judgments applied to `ActionOutcomeEstimate` facts.
//!
//! Invariants:
//! - Each policy is a pure function of (facts, minimal context).
//! - No side effects, no shared state, no caching.
//! - Signature: `fn name(facts, [target: UnitView], [caster: &CasterContext]) -> f32`.
//! - Policies are stateless and swappable (forward-compat for UnitQuirks / adaptation).
//!
//! Read by: factors (StepFactor / PlanFactor), critics, terminal eval, intent
//! scoring, agenda scorecards. Single source of truth for "how we value a fact."

pub mod cc;
pub mod damage;
pub mod env_severity;
pub mod friendly_fire;
pub mod heal;
pub mod status;

#[cfg(test)]
mod tests;
