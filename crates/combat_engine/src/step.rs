//! `step()` — the public engine entry point.
//!
//! Validates an action, expands it into an effect queue, then pumps effects
//! one at a time while scanning for reactions (AoOs) after each `MovePosition`
//! effect.
//!
//! ## Strict failure (decision 6.5)
//! If any `Damage` effect targets a unit that is already dead and that unit is
//! **not** the current action's actor, `step()` returns `Err(TargetGone)` and
//! rolls back state to entry. If the dead target *is* the actor (i.e. the mover
//! was killed by an earlier reaction), the effect is silently skipped — see the
//! actor-liveness truncation below.
//!
//! This branch is currently only reachable for Phase 2+ Cast/AoE actions where
//! one target in an AoE burst dies mid-burst and a follow-up effect targets
//! a different (also now dead) unit. For `Action::Move` the only Damage targets
//! are AoO victims (= the mover = the actor), so the non-actor branch cannot
//! trigger during Phase 0/1.
//!
//! ## Actor-liveness truncation
//! After each `MovePosition` effect is applied, reactions are processed one by
//! one via per-reaction sub-queues. Before expanding each reaction the mover's
//! liveness is checked: if the mover died from the previous reaction, the
//! remaining reactions for this step are skipped. No `ReactionFired` event is
//! emitted for skipped reactions and `reactions_left` on those enemies is not
//! decremented.
//!
//! Subsequent `MovePosition` effects for the same path are also skipped (the
//! dead-actor guard at the top of the main pump loop handles this).
//!
//! ## Reaction depth cap
//! A counter tracks how many reaction expansions have fired. Exceeding 100
//! returns `Err(ReactionDepthExceeded)` (state rolled back).

use std::collections::{HashMap, VecDeque};

use crate::{
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

            // Build occupancy map: alive non-actor units keyed by position.
            let actor_team = unit.team;
            let occupancy: HashMap<hexx::Hex, (crate::state::UnitId, crate::state::Team)> = state
                .units()
                .iter()
                .filter(|u| u.is_alive() && u.id != *actor)
                .map(|u| (u.pos, (u.id, u.team)))
                .collect();

            let last = path.len() - 1;
            for (i, &hex) in path.iter().enumerate() {
                if i == last {
                    if occupancy.contains_key(&hex) {
                        return Err(ActionError::DestinationOccupied { hex });
                    }
                } else if let Some(&(_, team)) = occupancy.get(&hex) {
                    if team != actor_team {
                        return Err(ActionError::PathBlockedByEnemy { hex });
                    }
                }
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
        // ── Dead-actor guard: skip remaining MovePositions when mover died ────
        if let Effect::MovePosition { actor, .. } = &effect {
            if !state.unit(*actor).is_some_and(|u| u.is_alive()) {
                continue;
            }
        }

        // ── Strict failure check (decision 6.5) ──────────────────────────────
        // Rollback for non-actor Damage targets; silently skip for the actor
        // (mid-action actor death is handled by actor-liveness truncation).
        // NOTE: in Phase 0/1 (Action::Move only) the sole Damage targets are
        // AoO victims which are always the mover (= actor_id), so the Err
        // branch below is reserved for Phase 2+ Cast/AoE actions.
        if let Effect::Damage { target, .. } = &effect {
            if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                if *target == actor_id {
                    continue; // actor died mid-action — skip silently
                }
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

        // After MovePosition: process reactions one at a time via per-reaction
        // sub-queues, with an actor-liveness check before each expansion.
        if let Effect::MovePosition { actor, to } = &effect {
            let new_pos = *to;
            let mover_id = *actor;

            let reactions = scan_reactions(state, mover_id, pos_before, new_pos, content);

            for reaction in reactions {
                // Actor died from a previous reaction this step — truncate chain.
                if !state.unit(mover_id).is_some_and(|u| u.is_alive()) {
                    break;
                }

                // Depth-cap: count reactions actually processed.
                reaction_depth += 1;
                if reaction_depth > REACTION_DEPTH_LIMIT {
                    return Err(ActionError::ReactionDepthExceeded);
                }

                // Emit ReactionFired only for reactions we actually expand.
                match &reaction {
                    Reaction::OpportunityAttack { from, victim } => {
                        events.push(Event::ReactionFired {
                            actor: *from,
                            kind: ReactionKind::OpportunityAttack,
                            against: *victim,
                        });
                    }
                }

                // Expand into a sub-queue and resolve fully (incl. derived
                // Damage→GainRage→Death) before pulling the next reaction.
                let mut sub_queue: VecDeque<Effect> =
                    expand_reaction(&reaction, content, rng).into_iter().collect();

                while let Some(sub_eff) = sub_queue.pop_front() {
                    // Strict failure check (decision 6.5) within sub-queue —
                    // keep for non-mover targets; skip silently for the mover.
                    if let Effect::Damage { target, .. } = &sub_eff {
                        if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                            if *target == mover_id {
                                continue;
                            }
                            return Err(ActionError::TargetGone);
                        }
                    }

                    let (sub_derived, sub_ctx) =
                        apply_effect(state, &sub_eff, content);

                    if let Some(ev) =
                        effect_to_event(&sub_eff, state, Some(pos_before), &sub_ctx)
                    {
                        events.push(ev);
                    }

                    for ef in sub_derived.into_iter().rev() {
                        sub_queue.push_front(ef);
                    }
                }
            }

            // Update prev_pos for the next move step.
            // (Irrelevant once the mover is dead, but harmless to advance.)
            prev_pos = new_pos;
        }

        // Derived effects (e.g. GainRage, Death from Damage in the main queue)
        // go to the front to preserve per-target ordering (decision 6.3).
        for ef in derived.into_iter().rev() {
            effect_queue.push_front(ef);
        }
    }

    // ── Emit ActionFinished ───────────────────────────────────────────────────

    events.push(Event::ActionFinished { action });

    Ok(events)
}
