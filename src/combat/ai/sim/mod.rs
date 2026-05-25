//! AI predictive simulation — parallel to engine `step()` for fast
//! scoring rollouts. Parity-tests against engine `targeting` and `step()`
//! continue to guarantee divergence detection.

pub mod effects_outcome;
pub mod effects_state;
