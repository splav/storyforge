//! Reaction types and AoO logic for Phase 0.
//!
//! `scan_reactions` checks which enemies can fire an AoO given a move step.
//! `expand_reaction` turns a pending `Reaction` into a list of `Effect`s.
//!
//! **AoO rule (mirrored from `combat/movement.rs`):**
//! - Attacker is an enemy, alive, has `reactions_left > 0`.
//! - Mover was *adjacent* to the attacker at `prev_pos`.
//! - Mover is *not adjacent* to the attacker at `new_pos`.
//! - Attacker has weapon dice available (via `ContentView::aoo_dice`).
//!
//! **No Bevy imports here** — decision 6.7.

use hexx::Hex;

use crate::{
    content::ContentView,
    effect::Effect,
    state::{CombatState, UnitId},
};

/// The kind of reaction that was triggered (used in `Event::ReactionFired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionKind {
    /// Opportunity attack — enemy leaves melee adjacency.
    OpportunityAttack,
}

/// A pending reaction to be expanded into effects.
#[derive(Debug, Clone)]
pub enum Reaction {
    OpportunityAttack { from: UnitId, victim: UnitId },
}

/// Scan for AoO reactions triggered when `mover` steps from `prev_pos` to
/// `new_pos`.
///
/// Returns one `Reaction::OpportunityAttack` per eligible enemy (i.e. every
/// enemy that was adjacent at `prev_pos`, is not adjacent at `new_pos`, has
/// `reactions_left > 0`, and has weapon dice in `content`).
///
/// Adjacency = hex distance of 1 (mirrors `unsigned_distance_to == 1`).
pub fn scan_reactions(
    state: &CombatState,
    mover_id: UnitId,
    prev_pos: Hex,
    new_pos: Hex,
    content: &dyn ContentView,
) -> Vec<Reaction> {
    let mover_team = match state.unit(mover_id) {
        Some(u) => u.team,
        None => return vec![],
    };

    let mut reactions = Vec::new();
    for enemy in state.units() {
        // Must be an opponent.
        if enemy.team == mover_team {
            continue;
        }
        // Must be alive.
        if !enemy.is_alive() {
            continue;
        }
        // Must have reactions left.
        if enemy.reactions_left <= 0 {
            continue;
        }
        // Must have AoO dice (weapon equipped).
        if content.aoo_dice(enemy.id).is_none() {
            continue;
        }
        // AoO rule: adjacent at prev_pos, NOT adjacent at new_pos.
        let was_adjacent = prev_pos.unsigned_distance_to(enemy.pos) == 1;
        let still_adjacent = new_pos.unsigned_distance_to(enemy.pos) == 1;
        if was_adjacent && !still_adjacent {
            reactions.push(Reaction::OpportunityAttack {
                from: enemy.id,
                victim: mover_id,
            });
        }
    }
    reactions
}

/// Expand a `Reaction` into the effects it produces.
///
/// For `OpportunityAttack`:
/// 1. `DecrementReactions { actor: from }` — spend the reaction.
/// 2. `Damage { target: victim, raw: dice.expected(), source: from, pierces: false }`
///    — the AoO hit.
///
/// Derived effects (`GainRage`, `Death`) come from `apply_effect(Damage)`.
///
/// Returns an empty vec if the attacker has no weapon dice (safety guard —
/// `scan_reactions` already filters these out under normal operation).
pub fn expand_reaction(
    reaction: &Reaction,
    content: &dyn ContentView,
    rng: &mut dyn crate::dice::DiceSource,
) -> Vec<Effect> {
    match reaction {
        Reaction::OpportunityAttack { from, victim } => {
            let Some(dice) = content.aoo_dice(*from) else {
                return vec![];
            };
            let raw = rng.roll(dice) as f32;
            vec![
                Effect::DecrementReactions { actor: *from },
                Effect::Damage {
                    target: *victim,
                    raw,
                    source: *from,
                    pierces: false,
                },
            ]
        }
    }
}
