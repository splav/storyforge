//! Read-only action preview (dry-run).
//!
//! Runs `action` against a clone of `state` using the stateless
//! [`ExpectedValue`] dice source, returning the events the real [`step`] would
//! emit — without mutating `state` or advancing any RNG.
//!
//! Because `ExpectedValue` always returns the analytical mean, the `1d20`
//! crit-fail check in `step` never fires (mean = 11 ≠ 1).  The returned events
//! therefore describe the **expected, non-crit-fail resolution**.  Crit-fail
//! probability is surfaced separately by the UI layer.

use crate::{
    action::{Action, ActionError},
    content::ContentView,
    dice::ExpectedValue,
    event::Event,
    state::CombatState,
    step::step,
};

/// Dry-run `action` against a clone of `state` using the stateless
/// `ExpectedValue` dice source, returning the events the real `step`
/// would emit — WITHOUT mutating `state` or advancing any RNG.
///
/// Because `ExpectedValue` rolls the analytical mean, the `1d20` crit-fail
/// roll never triggers: the returned events describe the expected
/// (non-crit-fail) resolution.  Crit-fail probability is surfaced elsewhere.
pub fn preview_action(
    state: &CombatState,
    action: Action,
    content: &dyn ContentView,
) -> Result<Vec<Event>, ActionError> {
    let mut sim = state.clone();
    match step(&mut sim, action, &mut ExpectedValue, content) {
        Ok((events, _ctx)) => Ok(events),
        Err(e) => Err(e),
    }
}
