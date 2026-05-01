//! Policy formulas — value judgments applied to `ActionOutcomeEstimate` facts.
//!
//! Invariants:
//! - Each policy is a pure function of (facts, minimal context).
//! - No side effects, no shared state, no caching.
//! - Signature: `fn name(facts, [target: &UnitSnapshot], [caster: &CasterContext]) -> f32`.
//! - Policies are stateless and swappable (forward-compat for UnitQuirks / adaptation).
//!
//! Read by: factors (StepFactor / PlanFactor), critics (step 10), terminal eval,
//! intent scoring, agenda scorecards (step 11). Single source of truth for "how
//! we value a fact."
//!
//! ## Extraction map (step 4.7)
//!
//! | Source location                                         | Policy                              |
//! |--------------------------------------------------------|-------------------------------------|
//! | `outcome::compute_score_core` — damage branch          | `damage::value`                     |
//! | `outcome::compute_score_core` — heal branch            | `heal::value`                       |
//! | `factors::offensive::friendly_fire_penalty` formula    | `friendly_fire::penalty`            |
//! | `scoring::stun_denial_value`                           | `status::stun_denial_value`         |
//! | `scoring::status_score`                                | `status::value`                     |
//! | composite (new in 4.7, consumer migration in 4.10)     | `cc::value`                         |

pub mod cc;
pub mod damage;
pub mod friendly_fire;
pub mod heal;
pub mod status;

#[cfg(test)]
mod tests;
