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
    ActionStarted {
        action: Action,
    },
    UnitMoved {
        actor: UnitId,
        from: Hex,
        to: Hex,
    },
    UnitDamaged {
        target: UnitId,
        source: crate::state::EffectSource,
        raw: f32,
        mitigation: i32,
        pierces: bool,
        amount: i32,
    },
    UnitHealed {
        target: UnitId,
        amount: i32,
    },
    StatusApplied {
        target: UnitId,
        status: StatusId,
    },
    StatusRemoved {
        target: UnitId,
        status: StatusId,
    },
    /// One DoT tick was applied to `target` from `status` originally cast by `source`.
    /// Emitted by `effect_to_event(TickDot)` when `dot_per_tick == 0` AND
    /// `hp_percent_dot == 0` (buff-only status tick with no damage component).
    StatusTicked {
        target: UnitId,
        status: StatusId,
        source: crate::state::EffectSource,
    },
    /// Fused event emitted when a DoT tick deals damage.  Replaces the previous
    /// `StatusTicked + UnitDamaged` pair with a single atomic event so consumers
    /// never need to correlate two events.
    ///
    /// `mitigation` is always 0 (DoT pierces armor). `pierces` is always true.
    DotDamaged {
        target: UnitId,
        source: crate::state::EffectSource,
        source_status: StatusId,
        raw: f32,
        mitigation: i32,
        pierces: bool,
        amount: i32,
    },
    /// Fused event when a HoT tick restores HP (the heal-over-time analogue of
    /// `DotDamaged`). A no-op tick (zero heal or already at max HP) emits
    /// `StatusTicked` instead, matching the zero-damage DoT convention.
    HotHealed {
        target: UnitId,
        source_status: StatusId,
        amount: i32,
    },
    ReactionFired {
        actor: UnitId,
        kind: ReactionKind,
        against: UnitId,
    },
    UnitDied {
        unit: UnitId,
    },
    /// Cast crit-failed.  Fired by `step()`'s `Action::Cast` arm immediately
    /// after the d20 roll lands on 1.  Subsequent aux effects (SelfDamage,
    /// ApplyStatus to caster) emit their own events; this one carries the
    /// *reason* (which `CritFailOutcome` fired) so the bridge can render
    /// the appropriate log line (`CriticalMiss` vs `CritFailSideEffect`).
    CritFailed {
        actor: UnitId,
        outcome: crate::content::CritFailOutcome,
    },
    ActionFinished {
        action: Action,
    },
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
    /// The actor's turn ended.  Emitted in three situations:
    /// 1. `step(Action::EndTurn)` — `cause: Manual`
    /// 2. `Effect::Death` of the current actor — `cause: DeathOfActor`
    /// 3. `Action::Cast` exhausts both AP and MP (S6 path) — `cause: ResourcesExhausted`
    ///
    /// Always emitted BEFORE the `Effect::AdvanceTurn` cascade runs, so the
    /// stream reads naturally: outgoing actor's turn ends → queue advances →
    /// skips/round wrap → next actor's turn starts.
    TurnEnded {
        actor: UnitId,
        cause: TurnEndCause,
    },
    /// The next actor's turn began.  Emitted immediately after `TurnEnded` (or
    /// after `RoundStarted` when the round wrapped).
    TurnStarted {
        actor: UnitId,
    },
    /// A unit's turn was skipped (dead or stunned).  Emitted from within the
    /// `Effect::AdvanceTurn` cascade, before `TurnEnded`/`TurnStarted`.
    TurnSkipped {
        actor: UnitId,
        reason: TurnSkipReason,
    },
    /// The round counter incremented and per-round resets fired.
    RoundStarted {
        round: u32,
    },
    /// A unit entered an aura's radius (or the aura source moved into range),
    /// causing `status_id` to become active on `target` from `source`.
    ///
    /// Emitted by `step()` as a diff between before/after `aura_membership_set`
    /// snapshots around `Effect::MovePosition` and `Effect::Death`.
    AuraStatusGained {
        target: UnitId,
        source: UnitId,
        status_id: StatusId,
    },
    /// A unit left an aura's radius (or the source moved away / died),
    /// causing `status_id` to no longer be active on `target` from `source`.
    AuraStatusLost {
        target: UnitId,
        source: UnitId,
        status_id: StatusId,
    },
    /// A boss entered a new phase, emitted after the `EnterPhase` effect cascade.
    /// The bridge translator reads this to write ECS-only deltas (name, abilities,
    /// AxisProfile, flavor, `EnemyPhases.pending` pop, `Dead` removal on revive).
    PhaseEntered {
        unit: UnitId,
        phase_idx: usize,
        prev_max_hp: i32,
        new_max_hp: i32,
    },
    /// A hazard on the grid triggered when `victim` stepped onto its hex.
    ///
    /// Emitted BEFORE the damage/status events that flow from the trap's
    /// `AbilityDef` fanout, so the event stream reads:
    ///   HazardTriggered → (optional EnvRevealed) → UnitDamaged / StatusApplied …
    HazardTriggered {
        env_id: crate::state::EnvId,
        victim: crate::state::UnitId,
    },

    /// An environment object became visible (by triggering or otherwise).
    /// Emitted alongside `HazardTriggered` when the object was not yet revealed
    /// before the trigger.
    EnvRevealed {
        env_id: crate::state::EnvId,
    },

    /// Unified pool-change event — the sole canonical pool-mutation event. Fires
    /// for every mutation of a unit's resource pool; `cause` carries the reason
    /// (Regen/Refill/Spent/Gained/MaxChanged).
    ///
    /// `PoolChangeCause::MaxChanged` is declared but not yet emitted — reserved
    /// for when `RefreshAggregates` propagates pool-max changes.
    PoolChanged {
        unit: UnitId,
        pool: crate::PoolKind,
        current: i32,
        max: i32,
        cause: crate::PoolChangeCause,
    },

    /// Initiative was rolled for `unit` at the start of a round.
    /// `total == roll + dex_mod`.  Emitted by `CombatState::roll_initiative_for_all`
    /// for every unit that is NOT preset.  Carried through the trace so the
    /// combat-log line "инициатива X: d20(roll) +dex_mod = total" can be rendered.
    InitiativeRolled {
        unit: UnitId,
        roll: i32,
        dex_mod: i32,
        total: i32,
    },
}

/// Why a unit's turn was skipped in `Effect::AdvanceTurn`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSkipReason {
    Dead,
    Stunned,
}

/// Why the actor's turn ended.  Carried by `Event::TurnEnded` so consumers
/// (bridge, replay, AI log) can distinguish the three paths without pattern-
/// matching the surrounding event stream.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndCause {
    /// `Action::EndTurn` was submitted explicitly (player / AI pressed end-turn).
    Manual,
    /// The current actor died mid-action; `Effect::Death` derived `AdvanceTurn`
    /// which force-ended their turn.
    DeathOfActor,
    /// `Action::Cast` left AP=0 **and** MP=0; the engine's S6 path auto-ended
    /// the turn without a second `step()` call.
    ResourcesExhausted,
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
        Effect::MovePosition { actor, to } => Some(Event::UnitMoved {
            actor: *actor,
            from: prev_pos.unwrap_or(*to),
            to: *to,
        }),
        Effect::DecrementMP { .. } => None,
        Effect::DecrementAP { .. } => None,
        Effect::Damage { target, source, .. } => {
            let d = ctx
                .damage
                .as_ref()
                .expect("Damage effect must populate ApplyCtx.damage");
            Some(Event::UnitDamaged {
                target: *target,
                source: *source,
                raw: d.raw,
                mitigation: d.mitigation,
                pierces: d.pierces,
                amount: d.final_amount,
            })
        }
        Effect::Heal { target, .. } => Some(Event::UnitHealed {
            target: *target,
            amount: ctx.heal_amount.unwrap_or(0),
        }),
        Effect::PayCost { .. } => {
            // Pool events are emitted via ctx.pool_events, not effect_to_event.
            None
        }
        Effect::ApplyStatus { target, status, .. } => Some(Event::StatusApplied {
            target: *target,
            status: status.clone(),
        }),
        Effect::RemoveStatus { target, status } => Some(Event::StatusRemoved {
            target: *target,
            status: status.clone(),
        }),
        Effect::GainRage { .. } => {
            // Pool events are emitted via ctx.pool_events, not effect_to_event.
            None
        }
        // Pool events emitted via ctx.pool_events, not effect_to_event.
        Effect::RestorePool { .. } => None,
        Effect::DecrementReactions { .. } => None,
        Effect::Death { unit } => Some(Event::UnitDied { unit: *unit }),
        Effect::RefreshAggregates { .. } => None,
        Effect::TickDot { target, status } => {
            if let Some(dot) = &ctx.dot_damage {
                // Damaging tick: emit fused DotDamaged.
                Some(Event::DotDamaged {
                    target: *target,
                    source: dot.source,
                    source_status: dot.source_status.clone(),
                    raw: dot.raw,
                    mitigation: dot.mitigation,
                    pierces: dot.pierces,
                    amount: dot.final_amount,
                })
            } else {
                // Zero-damage tick (buff-only status): emit StatusTicked.
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
        }
        Effect::TickHeal { target, status } => {
            if let Some(hot) = &ctx.hot_heal {
                // Healing tick: emit fused HotHealed.
                Some(Event::HotHealed {
                    target: *target,
                    source_status: hot.source_status.clone(),
                    amount: hot.amount,
                })
            } else {
                // Zero-heal tick (heal_per_tick == 0 or already at max): emit StatusTicked.
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
        }
        Effect::ExpireStatus { .. } => None,
        Effect::Spawn {
            summoner,
            template_id,
            ..
        } => {
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
        // Passive reveal: no event for the range scan, only for the individual reveal.
        Effect::RevealEnvInRange { .. } => None,
        // Individual reveal: emit EnvRevealed only if the env was newly revealed
        // (ctx.env_revealed is false when idempotent no-op).
        Effect::RevealEnv { id, .. } => {
            if ctx.env_revealed {
                Some(Event::EnvRevealed { env_id: *id })
            } else {
                None
            }
        }

        // TurnSkipped events flow via ctx.turn_skip_events drained by the pump loop.
        Effect::AdvanceTurn => None,
        // state.round was already incremented in BumpRound's apply arm before
        // effect_to_event is called, so this reflects the new round number.
        Effect::BumpRound => Some(Event::RoundStarted { round: state.round }),
        // Phase-transition atomics (4d): SetMaxHp produces no observable event;
        // EnterPhase produces PhaseEntered.
        Effect::SetMaxHp { .. } => None,
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
