//! `Event` enum — observable facts emitted by the engine for UI/log/replay.

use hexx::Hex;

use crate::{
    action::Action,
    effect::{ApplyCtx, Effect, SpawnBlockedReason},
    reaction::ReactionKind,
    state::{CombatState, Team, UnitId},
    StatusId,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    ActionStarted { action: Action },
    UnitMoved { actor: UnitId, from: Hex, to: Hex },
    UnitDamaged {
        target: UnitId,
        source: UnitId,
        raw: f32,
        mitigation: i32,
        pierces: bool,
        amount: i32,
    },
    UnitHealed { target: UnitId, amount: i32 },
    StatusApplied { target: UnitId, status: StatusId },
    StatusRemoved { target: UnitId, status: StatusId },
    /// One DoT tick was applied to `target` from `status` originally cast by `source`.
    /// Fires BEFORE the derived `UnitDamaged` event so log can render "поражён ядом"
    /// then damage breakdown.
    StatusTicked { target: UnitId, status: StatusId, source: UnitId },
    RageGained { unit: UnitId, current: i32, max: i32 },
    ReactionFired { actor: UnitId, kind: ReactionKind, against: UnitId },
    UnitDied { unit: UnitId },
    /// Cast crit-failed.  Fired by `step()`'s `Action::Cast` arm immediately
    /// after the d20 roll lands on 1.  Subsequent aux effects (SelfDamage,
    /// ApplyStatus to caster) emit their own events; this one carries the
    /// *reason* (which `CritFailOutcome` fired) so the bridge can render
    /// the appropriate log line (`CriticalMiss` vs `CritFailSideEffect`).
    CritFailed { actor: UnitId, outcome: crate::content::CritFailOutcome },
    ActionFinished { action: Action },
    ManaRegenerated { unit: UnitId, current: i32, max: i32 },
    EnergyRegenerated { unit: UnitId, current: i32, max: i32 },
    UnitSpawned {
        uid: UnitId,
        summoner: UnitId,
        pos: hexx::Hex,
        template_id: String,
        team: Team,
    },
    SpawnBlocked {
        summoner: UnitId,
        template_id: String,
        reason: SpawnBlockedReason,
    },
    /// The actor's turn ended.  Emitted by `step(Action::EndTurn)` BEFORE the
    /// `Effect::AdvanceTurn` cascade runs, so the stream reads naturally:
    /// outgoing actor's turn ends → queue advances → skips/round wrap → next
    /// actor's turn starts.
    TurnEnded { actor: UnitId },
    /// The next actor's turn began.  Emitted immediately after `TurnEnded` (or
    /// after `RoundStarted` when the round wrapped).
    TurnStarted { actor: UnitId },
    /// A unit's turn was skipped (dead or stunned).  Emitted from within the
    /// `Effect::AdvanceTurn` cascade, before `TurnEnded`/`TurnStarted`.
    TurnSkipped { actor: UnitId, reason: TurnSkipReason },
    /// The round counter incremented and per-round resets fired.
    RoundStarted { round: u32 },
    /// A unit entered an aura's radius (or the aura source moved into range),
    /// causing `status_id` to become active on `target` from `source`.
    ///
    /// Emitted by `step()` as a diff between before/after `aura_membership_set`
    /// snapshots around `Effect::MovePosition` and `Effect::Death`.
    AuraStatusGained { target: UnitId, source: UnitId, status_id: StatusId },
    /// A unit left an aura's radius (or the source moved away / died),
    /// causing `status_id` to no longer be active on `target` from `source`.
    AuraStatusLost { target: UnitId, source: UnitId, status_id: StatusId },
    /// A boss entered a new phase.  Emitted by `apply_effect(EnterPhase)`
    /// after the cascade (SetMaxHp, SetArmor, SetBaseSpeed, Heal,
    /// RefreshAggregates) is derived.
    ///
    /// Bridge translator reads this to write ECS-only deltas (name, abilities,
    /// AxisProfile, flavor text, `pop_front()` on `EnemyPhases.pending`,
    /// remove `Dead` if `heal_to_full` revived the unit).
    PhaseEntered {
        unit: UnitId,
        phase_idx: usize,
        prev_max_hp: i32,
        new_max_hp: i32,
    },
}

/// Why a unit's turn was skipped in `Effect::AdvanceTurn`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSkipReason {
    Dead,
    Stunned,
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
        Effect::DecrementAP { .. } => None,
        Effect::Damage { target, source, .. } => {
            let d = ctx.damage.as_ref().expect("Damage effect must populate ApplyCtx.damage");
            Some(Event::UnitDamaged {
                target: *target,
                source: *source,
                raw: d.raw,
                mitigation: d.mitigation,
                pierces: d.pierces,
                amount: d.final_amount,
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
        Effect::TickDot { target, status } => {
            state.unit(*target).and_then(|u| {
                u.statuses
                    .iter()
                    .find(|s| s.id == *status)
                    .map(|s| Event::StatusTicked {
                        target: *target,
                        status: status.clone(),
                        source: s.applier,
                    })
            })
        }
        Effect::ExpireStatus { .. } => None,
        Effect::Spawn { summoner, template_id, .. } => {
            if let Some(reason) = ctx.spawn_blocked.clone() {
                return Some(Event::SpawnBlocked {
                    summoner: *summoner,
                    template_id: template_id.clone(),
                    reason,
                });
            }
            let uid = ctx.spawn_uid?;
            let pos = ctx.spawn_pos?;
            let team = state.unit(uid).map(|u| u.team)?;
            Some(Event::UnitSpawned {
                uid,
                summoner: *summoner,
                pos,
                template_id: template_id.clone(),
                team,
            })
        }
        // TurnSkipped events flow via ctx.turn_skip_events drained by the pump loop.
        Effect::AdvanceTurn => None,
        // state.round was already incremented in BumpRound's apply arm before
        // effect_to_event is called, so this reflects the new round number.
        Effect::BumpRound => Some(Event::RoundStarted { round: state.round }),
        // Phase-transition atomics (4d): SetMaxHp/SetArmor/SetBaseSpeed produce
        // no observable events; EnterPhase produces PhaseEntered.
        Effect::SetMaxHp { .. } | Effect::SetArmor { .. } | Effect::SetBaseSpeed { .. } => None,
        Effect::EnterPhase { unit, phase_idx } => {
            let (prev_max_hp, new_max_hp) = ctx.phase_entered.unwrap_or((0, 0));
            Some(Event::PhaseEntered {
                unit: *unit,
                phase_idx: *phase_idx,
                prev_max_hp,
                new_max_hp,
            })
        }
    }
}
