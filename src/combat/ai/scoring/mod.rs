//! Scoring — facts → numbers.
//!
//! Collects everything that converts raw outcome facts into f32 scores:
//! - `horizon` — DPR helpers and damage-horizon estimates.
//! - `target_selection` — relative enemy ranking score.
//! - `position_eval` — tile desirability by role profile.
//! - `trade` — unit value and trade delta.
//! - `policy` — HP-equivalent value formulas (damage, heal, CC, status).

pub mod factors;
pub mod horizon;
pub mod policy;
pub mod position_eval;
pub mod target_selection;
pub mod trade;

// Re-export the public API of horizon so that existing callers
// `crate::combat::ai::scoring::{applies_cc, …}` continue to work
// without path changes.
pub use horizon::{
    applies_cc, estimate_damage_horizon, estimate_st_damage, horizon_avg,
    horizon_window_sum, status_applications, stun_denial_value,
};
