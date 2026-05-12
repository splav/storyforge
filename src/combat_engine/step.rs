//! `step()` — the public engine entry point.
//!
//! Validates an action, expands it into an effect queue, then pumps effects
//! one at a time while scanning for reactions (AoOs) after each `MovePosition`
//! effect.
//!
//! ## Strict failure (decision 6.5)
//! If any effect targeting a living unit finds that unit already dead,
//! `step()` returns `Err(TargetGone)` and the state is rolled back to what it
//! was at entry (via a clone taken before any mutation).
//!
//! ## Reaction depth cap
//! A counter tracks how many reaction expansions have fired. Exceeding 100
//! returns `Err(ReactionDepthExceeded)` (state rolled back).

use std::collections::VecDeque;

use crate::combat_engine::{
    action::{Action, ActionError},
    content::ContentView,
    dice::DiceSource,
    effect::{apply_effect, Effect},
    event::{effect_to_event, Event},
    reaction::{expand_reaction, scan_reactions, Reaction, ReactionKind},
    state::CombatState,
};

const REACTION_DEPTH_LIMIT: usize = 100;

/// Advance `state` by one action.
///
/// Returns the ordered list of events that occurred, or an error if the action
/// was illegal or a strict-failure condition was hit (see `ActionError`).
///
/// State is rolled back (no mutation) on any error.
pub fn step(
    state: &mut CombatState,
    action: Action,
    rng: &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<Vec<Event>, ActionError> {
    // Clone state at entry for rollback on error (decision 6.5).
    let snapshot = state.clone();

    let result = step_inner(state, action, rng, content);

    if result.is_err() {
        *state = snapshot;
    }
    result
}

fn step_inner(
    state: &mut CombatState,
    action: Action,
    rng: &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<Vec<Event>, ActionError> {
    let mut events: Vec<Event> = Vec::new();
    let mut effect_queue: VecDeque<Effect> = VecDeque::new();
    let mut reaction_depth: usize = 0;

    // ── Pre-validate ──────────────────────────────────────────────────────────

    match &action {
        Action::Move { actor, path } => {
            let unit = state.unit(*actor).ok_or(ActionError::UnknownActor)?;
            if !unit.is_alive() {
                return Err(ActionError::UnknownActor);
            }
            if path.is_empty() {
                return Err(ActionError::NoPath);
            }
            if path.len() as i32 > unit.movement_points {
                return Err(ActionError::OutOfMP);
            }
        }
    }

    // ── Emit ActionStarted event ──────────────────────────────────────────────

    events.push(Event::ActionStarted { action: action.clone() });

    // ── Expand action into initial effect queue ───────────────────────────────

    match &action {
        Action::Move { actor, path } => {
            effect_queue.push_back(Effect::DecrementMP {
                actor: *actor,
                by: path.len() as i32,
            });
            for &hex in path {
                effect_queue.push_back(Effect::MovePosition { actor: *actor, to: hex });
            }
        }
    }

    // ── Pump loop ─────────────────────────────────────────────────────────────
    //
    // We track the actor's "previous position" so that each MovePosition step
    // can tell the AoO scanner where the mover came from.

    let actor_id = match &action {
        Action::Move { actor, .. } => *actor,
    };
    // prev_pos starts as the actor's position before any effects are applied.
    let mut prev_pos = state.unit(actor_id).map(|u| u.pos).unwrap_or_default();

    while let Some(effect) = effect_queue.pop_front() {
        // ── Strict failure check (decision 6.5) ──────────────────────────────
        if let Effect::Damage { target, .. } = &effect {
            if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                return Err(ActionError::TargetGone);
            }
        }

        // Capture the actor's position before MovePosition updates it.
        // For non-move effects this is unused but harmless — always prev_pos.
        let pos_before = prev_pos;

        // Apply the effect.
        let (derived, ctx) = apply_effect(state, &effect, content);

        // Emit the corresponding event.
        if let Some(ev) = effect_to_event(&effect, state, Some(pos_before), &ctx) {
            events.push(ev);
        }

        // After MovePosition: update prev_pos, then scan for AoO reactions.
        if let Effect::MovePosition { actor, to } = &effect {
            let new_pos = *to;
            // prev_pos for this step was pos_before.
            let reactions = scan_reactions(state, *actor, pos_before, new_pos, content);

            // Emit ReactionFired events.
            for reaction in &reactions {
                match reaction {
                    Reaction::OpportunityAttack { from, victim } => {
                        events.push(Event::ReactionFired {
                            actor: *from,
                            kind: ReactionKind::OpportunityAttack,
                            against: *victim,
                        });
                    }
                }
            }

            // Check depth before expanding any reactions.
            if !reactions.is_empty() {
                reaction_depth += reactions.len();
                if reaction_depth > REACTION_DEPTH_LIMIT {
                    return Err(ActionError::ReactionDepthExceeded);
                }
            }

            // Expand reactions and prepend to the queue (so they resolve before
            // the next move step).
            let mut reaction_effects: Vec<Effect> = Vec::new();
            for reaction in &reactions {
                reaction_effects.extend(expand_reaction(reaction, content, rng));
            }
            // Prepend in order: first reaction's effects first.
            for ef in reaction_effects.into_iter().rev() {
                effect_queue.push_front(ef);
            }

            // Update prev_pos for the next move step.
            prev_pos = new_pos;
        }

        // Derived effects (e.g. GainRage, Death from Damage) go to the front
        // to preserve per-target ordering (decision 6.3).
        for ef in derived.into_iter().rev() {
            effect_queue.push_front(ef);
        }
    }

    // ── Emit ActionFinished ───────────────────────────────────────────────────

    events.push(Event::ActionFinished { action });

    Ok(events)
}
