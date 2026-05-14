//! `Event` enum — observable facts emitted by the engine for UI/log/replay.

use hexx::Hex;

use crate::{
    action::Action,
    effect::{ApplyCtx, Effect},
    reaction::ReactionKind,
    state::{CombatState, UnitId},
    StatusId,
};

/// A domain-level fact produced by `step()`.  Consumers (UI, logger, replay)
/// subscribe to this stream; they never write back to state.
#[derive(Debug, Clone)]
pub enum Event {
    ActionStarted { action: Action },
    UnitMoved { actor: UnitId, from: Hex, to: Hex },
    UnitDamaged { target: UnitId, amount: f32, source: UnitId },
    UnitHealed { target: UnitId, amount: i32 },
    StatusApplied { target: UnitId, status: StatusId },
    StatusRemoved { target: UnitId, status: StatusId },
    RageGained { unit: UnitId, current: i32, max: i32 },
    ReactionFired { actor: UnitId, kind: ReactionKind, against: UnitId },
    UnitDied { unit: UnitId },
    ActionFinished { action: Action },
}

/// Convert an effect (post-application) to an `Event`.
///
/// `prev_pos` is required for `MovePosition` (the position before the effect
/// was applied — not recoverable from state afterwards).
///
/// Returns `None` for effects that produce no observable event (e.g.
/// `RefreshAggregates`, `DecrementReactions`, `DecrementMP`).
pub fn effect_to_event(
    effect: &Effect,
    state: &CombatState,
    prev_pos: Option<Hex>,
    ctx: &ApplyCtx,
) -> Option<Event> {
    match effect {
        Effect::MovePosition { actor, to } => {
            Some(Event::UnitMoved {
                actor: *actor,
                from: prev_pos.unwrap_or(*to),
                to: *to,
            })
        }
        Effect::DecrementMP { .. } => None,
        Effect::Damage { target, source, .. } => {
            Some(Event::UnitDamaged {
                target: *target,
                amount: ctx.final_damage.unwrap_or(0.0),
                source: *source,
            })
        }
        Effect::Heal { target, .. } => {
            Some(Event::UnitHealed {
                target: *target,
                amount: ctx.heal_amount.unwrap_or(0),
            })
        }
        Effect::PayCost { .. } => None,
        Effect::ApplyStatus { target, status, .. } => {
            Some(Event::StatusApplied {
                target: *target,
                status: status.clone(),
            })
        }
        Effect::RemoveStatus { target, status } => {
            Some(Event::StatusRemoved {
                target: *target,
                status: status.clone(),
            })
        }
        Effect::GainRage { target } => {
            if let Some(u) = state.unit(*target) {
                if let Some((current, max)) = u.rage {
                    return Some(Event::RageGained { unit: *target, current, max });
                }
            }
            None
        }
        Effect::DecrementReactions { .. } => None,
        Effect::Death { unit } => Some(Event::UnitDied { unit: *unit }),
        Effect::RefreshAggregates { .. } => None,
    }
}
