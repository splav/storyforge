//! Bevy-side action-legality gate. Wires `combat::actions::check_legality`
//! against live ECS queries; rejected actions still end the turn to keep
//! the pipeline forward-moving.
//!
//! All substantive rules live in `combat::actions`; this file is a thin
//! adapter that translates between Bevy components and the `ActionState`
//! trait. See `combat/actions/mod.rs` for the rule list.

use crate::combat::actions::{
    check_legality, ActionState, ActorView, IllegalReason, ProposedAction,
};
use crate::content::content_view::{ActiveContent, ContentView};
use crate::game::components::{ActiveCombatant, ValidationActorQ, ValidationTargetQ};
use crate::game::messages::{EndTurn, UseAbility, ValidatedAction};
use crate::game::resources::HexPositions;
use bevy::prelude::*;

#[allow(clippy::too_many_arguments)]
pub fn validate_action_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    content: Res<ActiveContent>,
    positions: Res<HexPositions>,
    mut events: MessageReader<UseAbility>,
    actors: Query<ValidationActorQ>,
    targets: Query<ValidationTargetQ>,
    mut validated: MessageWriter<ValidatedAction>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        // Turn-ownership is outside the legality layer's scope — gate it
        // here. A stray `UseAbility` from a non-current actor is silently
        // dropped (no EndTurn to avoid ending the real current actor's turn).
        if active != Some(ev.actor) {
            continue;
        }

        let state = BevyActions {
            content: &content,
            positions: &positions,
            actors: &actors,
            targets: &targets,
        };
        let proposal = ProposedAction {
            actor: ev.actor,
            ability: &ev.ability,
            target: ev.target,
            target_pos: ev.target_pos,
        };
        match check_legality(proposal, &state) {
            Ok(outcome) => {
                validated.write(ValidatedAction {
                    actor: ev.actor,
                    ability: ev.ability.clone(),
                    target: ev.target,
                    target_pos: ev.target_pos,
                    disadvantage: outcome.disadvantage,
                });
            }
            Err(_reason) => {
                // Rejected action still ends the turn to prevent infinite
                // loops from a stuck command source. (Could in principle
                // fork by reason — e.g. keep turn on UnknownAbility which
                // smells like a content bug — but current callers only
                // emit well-formed events; fail-forward is fine.)
                end_turn.write(EndTurn { actor: ev.actor });
            }
        }
    }
}

// ── Bevy adapter ───────────────────────────────────────────────────────────

/// `ActionState` impl over live ECS queries. Holds references with a single
/// named lifetime `'a` — every borrow taken from the system's parameters
/// lives at least as long as the adapter, which is built and consumed
/// inside one `validate_action_system` iteration.
///
/// No borrows leak out through the trait: `actor_view` returns an owned
/// `ActorView` copy, `actor_knows_ability` answers a direct bool. That
/// sidesteps Bevy's `Query` fetch-item lifetime (which ends when `.get()`
/// returns) — we never try to hand a `&'static` slice out.
struct BevyActions<'w, 's, 'a> {
    content: &'a ContentView,
    positions: &'a HexPositions,
    actors: &'a Query<'w, 's, ValidationActorQ>,
    targets: &'a Query<'w, 's, ValidationTargetQ>,
}

impl ActionState for BevyActions<'_, '_, '_> {
    fn content(&self) -> &ContentView {
        self.content
    }

    fn actor_view(&self, actor: Entity) -> Option<ActorView> {
        let pos = self.positions.get(&actor)?;
        let a = self.actors.get(actor).ok()?;
        let (causes_disadvantage, blocks_mana_abilities) = match a.statuses {
            Some(se) => se.0.iter().fold((false, false), |(d, m), s| {
                let def = self.content.statuses.get(&s.id);
                (
                    d || def.is_some_and(|x| x.causes_disadvantage),
                    m || def.is_some_and(|x| x.blocks_mana_abilities),
                )
            }),
            None => (false, false),
        };
        Some(ActorView {
            pos,
            hp: a.vital.hp,
            ap: a.ap.action_points,
            mana: a.mana.map(|m| m.current),
            rage: a.rage.map(|r| r.current),
            energy: a.energy.map(|e| e.current),
            causes_disadvantage,
            blocks_mana_abilities,
            is_alive: a.vital.is_alive(),
        })
    }

    fn actor_knows_ability(&self, actor: Entity, ability: &crate::core::AbilityId) -> bool {
        self.actors
            .get(actor)
            .map(|a| a.abilities.0.contains(ability))
            .unwrap_or(false)
    }

    fn is_target_alive(&self, target: Entity) -> Option<bool> {
        self.targets.get(target).ok().map(|t| t.vital.is_alive())
    }
}

// UI tooltips will eventually surface IllegalReason. For now validation only
// needs the reject-or-accept bit; the import is kept so the module re-exports
// the enum for downstream wiring without a second `pub use`.
#[allow(dead_code)]
const _: fn(IllegalReason) = |_| {};
