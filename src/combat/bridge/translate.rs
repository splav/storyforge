//! Engine `Event` → Bevy-land translation (`translate_one` exhaustive match).

use bevy::prelude::*;

use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{
    CombatEvent, CombatLog, CritFailOutcomeEcs, SpawnBlockedReasonEcs, TurnSkipReasonEcs,
};
use crate::game::hex::LAYOUT;
use crate::ui::hex_grid::HexGridOffset;

use super::*;
use combat_engine::{event::Event, reaction::ReactionKind, state::UnitId};

// ── translate_events: unified bridge translator ───────────────────────────────

/// Cast-flow context — marker that the current translate_events call is
/// processing an `Action::Cast` event stream.
///
/// Cast-specific events (`UnitHealed`, `StatusApplied`, `CritFailed`,
/// `SpawnBlocked`) use `ctx.cast.is_some()` as a discriminant.
/// `Event::UnitSpawned` is handled in a separate post-pass at the callsite
/// because it requires `&mut Commands` which cannot be stored in `TranslateCtx`
/// without propagating Bevy's system-scoped `Commands` lifetime.
pub(crate) struct CastCtx {
    // Marker struct — no fields needed; cast-specific behavior is gated on
    // ctx.cast.is_some() inside translate_one.
    pub(crate) _phantom: (),
}

/// Move-flow context — fields only needed when translating `Action::Move` events.
pub(crate) struct MoveCtx<'a> {
    pub(crate) actor: Entity,
    pub(crate) combat_state: &'a CombatStateRes,
    pub(crate) grid_offset: &'a HexGridOffset,
    /// Aggregated start position for the single `UnitMoved` log entry.
    pub(crate) first_from: Option<hexx::Hex>,
    /// Aggregated end position for the single `UnitMoved` log entry.
    pub(crate) last_to: Option<hexx::Hex>,
    /// All waypoints (world-space) for the movement animation.
    pub(crate) waypoints: Vec<Vec2>,
    /// State machine for AoO pairing: `ReactionFired` immediately precedes the
    /// paired `UnitDamaged` in the event stream (decision 6.3).
    /// PRESERVE: do not fuse into `Event::AooDamaged` here — deferred to a
    /// future S-task (the second fusion candidate after S5's DotDamaged).
    pub(crate) pending_aoo_target: Option<UnitId>,
}

/// Bundle of all mutable state shared across `translate_events`.
///
/// The four formerly-separate translator functions each closed over a different
/// subset of this state; now one exhaustive `match` in `translate_one` branches
/// on `ctx.cast` / `ctx.move_` presence to recover the same context-dependent
/// behaviour.
///
/// Lifetime `'a` is the lifetime of the Bevy system parameter borrows passed in
/// from `process_action_system` / `bootstrap_combat_state`.
pub(crate) struct TranslateCtx<'a> {
    /// Shared by every translator.  Held as `&mut` so the `UnitSpawned` arm
    /// can pass it to `spawn_ecs_entity_from_engine_unit` (which registers the
    /// new entity).  Read-only arms dereference via `&*ctx.id_map`.
    pub(crate) log: &'a mut CombatLog,
    pub(crate) id_map: &'a mut UnitIdMap,
    /// Consolidated bridge queues: deaths and turn_lifecycle are written here
    /// during translation; animations and phases are written in post-passes
    /// directly on the `ResMut<BridgeQueues>`.
    pub(crate) queues: &'a mut BridgeQueues,
    /// Cast-flow-specific state (None outside `Action::Cast` translation).
    pub(crate) cast: Option<CastCtx>,
    /// Move-flow-specific state (None outside `Action::Move` translation).
    pub(crate) move_: Option<MoveCtx<'a>>,
}

/// Unified bridge event translator — one exhaustive `match` over every
/// `Event` variant.
///
/// Replaces four formerly-separate translator functions:
/// - `translate_tick_events`     (dot-damage, death, rage, mana-regen)
/// - `translate_end_turn_events` (turn/round lifecycle, aura status changes)
/// - `translate_cast_events`     (ability log entry, heal, status, crit-fail, spawn)
/// - `translate_move_events`     (waypoint aggregation, AoO pairing, movement anim)
///
/// Context-dependent behaviour (cast vs move vs tick) is driven by the
/// presence of `ctx.cast` / `ctx.move_` sub-structs:
///
/// - `UnitDamaged` in tick context: pierce-aware `armor_reduced` formula.
///   In cast context: passes `mitigation` as-is (engine zeroes it for piercing
///   casts). In move context: only handled when paired with a preceding
///   `ReactionFired` (AoO state machine).
/// - `UnitMoved`, `ReactionFired`: only meaningful in move context.
/// - `CritFailed`, `UnitSpawned`, `SpawnBlocked`, `UnitHealed`, `StatusApplied`:
///   only meaningful in cast context.
/// - Turn/round/aura events: always meaningful (B5 can emit them in any flow).
///
/// After the loop, callers in move context must call `finalize_move` to emit
/// the aggregated `UnitMoved` log entry and enqueue the movement animation.
pub(crate) fn translate_events(events: &[Event], ctx: &mut TranslateCtx<'_>) {
    for ev in events {
        translate_one(ev, ctx);
    }
}

#[allow(clippy::too_many_lines)]
fn translate_one(ev: &Event, ctx: &mut TranslateCtx<'_>) {
    match ev {
        // ── Move-specific: position tracking ─────────────────────────────────
        Event::UnitMoved { from, to, .. } => {
            // no-op: not produced during Cast or tick actions
            if let Some(mv) = ctx.move_.as_mut() {
                if mv.first_from.is_none() {
                    mv.first_from = Some(*from);
                    mv.waypoints
                        .push(LAYOUT.hex_to_world_pos(*from) + mv.grid_offset.0);
                }
                mv.last_to = Some(*to);
                mv.waypoints
                    .push(LAYOUT.hex_to_world_pos(*to) + mv.grid_offset.0);
            }
        }

        // ── Move-specific: AoO state machine ─────────────────────────────────
        Event::ReactionFired { kind, against, .. } => {
            // AoO reactions set the pending target for the next UnitDamaged pair.
            // Non-AoO reactions have no bridge representation yet.
            if matches!(kind, ReactionKind::OpportunityAttack) {
                if let Some(mv) = ctx.move_.as_mut() {
                    mv.pending_aoo_target = Some(*against);
                }
            }
        }

        // ── UnitDamaged: three context-dependent behaviours ───────────────────
        //
        // Move:  only AoO-paired damage (decision 6.3 — pending_aoo_target machine).
        // Cast:  pass mitigation as-is (engine zeroes it for piercing casts).
        // Tick:  pierce-aware formula (DoT always pierces armor — use pierces flag).
        Event::UnitDamaged {
            target,
            amount,
            raw,
            mitigation,
            pierces,
            source,
        } => {
            if let Some(mv) = ctx.move_.as_mut() {
                if mv.pending_aoo_target == Some(*target) {
                    // AoO arm: source is always a unit (reactions are unit-only).
                    let source_uid = match source {
                        combat_engine::state::EffectSource::Unit(u) => *u,
                        // An Env source cannot be an AoO attacker; fall through to
                        // the non-AoO env-damage branch below.
                        combat_engine::state::EffectSource::Env(_) => {
                            // Clear pending — this is not the expected AoO damage.
                            mv.pending_aoo_target = None;
                            // Fall through to env-damage log below.
                            let armor_reduced = if *pierces { 0 } else { *mitigation };
                            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                                ctx.log.push(CombatEvent::DamageResult {
                                    target: tgt_ent,
                                    raw: raw.round() as i32,
                                    armor_reduced,
                                    final_damage: *amount,
                                });
                            }
                            return;
                        }
                    };
                    let Some(attacker_ent) = ctx.id_map.get_entity(source_uid) else {
                        mv.pending_aoo_target = None;
                        return;
                    };
                    let Some(target_ent) = ctx.id_map.get_entity(*target) else {
                        mv.pending_aoo_target = None;
                        return;
                    };
                    let killed = mv
                        .combat_state
                        .0
                        .unit(*target)
                        .map(|u| !u.is_alive())
                        .unwrap_or(false);
                    ctx.log.push(CombatEvent::OpportunityAttack {
                        attacker: attacker_ent,
                        target: target_ent,
                        damage: *amount,
                        killed,
                    });
                    mv.pending_aoo_target = None;
                } else {
                    // Non-AoO damage during Move: only env (trap) damage reaches
                    // here.  Log it so HP/UI stay consistent; no attacker entity.
                    let armor_reduced = if *pierces { 0 } else { *mitigation };
                    if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                        ctx.log.push(CombatEvent::DamageResult {
                            target: tgt_ent,
                            raw: raw.round() as i32,
                            armor_reduced,
                            final_damage: *amount,
                        });
                    }
                }
            } else if ctx.cast.is_some() {
                // Cast context: engine already zeroes mitigation for piercing casts.
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
                ctx.log.push(CombatEvent::DamageResult {
                    target: tgt_ent,
                    raw: raw.round() as i32,
                    armor_reduced: *mitigation,
                    final_damage: *amount,
                });
            } else {
                // Tick context: apply pierce-aware formula.
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    let armor_reduced = if *pierces { 0 } else { *mitigation };
                    ctx.log.push(CombatEvent::DamageResult {
                        target: tgt_ent,
                        raw: raw.round() as i32,
                        armor_reduced,
                        final_damage: *amount,
                    });
                }
            }
        }

        // ── DoT damage (fused atomic, tick context only) ──────────────────────
        Event::DotDamaged {
            target,
            source,
            source_status,
            raw,
            mitigation,
            pierces,
            amount,
        } => {
            // no-op: DotDamaged not produced during Cast or Move actions
            if ctx.cast.is_none() && ctx.move_.is_none() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
                // For env-applied DoTs there is no unit attacker; source is None.
                let src_ent_opt: Option<Entity> = match source {
                    combat_engine::state::EffectSource::Unit(u) => ctx.id_map.get_entity(*u),
                    combat_engine::state::EffectSource::Env(_) => None,
                };
                ctx.log.push(CombatEvent::DotDamaged {
                    target: tgt_ent,
                    source: src_ent_opt,
                    source_status: source_status.clone(),
                    raw: *raw,
                    mitigation: *mitigation,
                    pierces: *pierces,
                    amount: *amount,
                });
            }
        }

        // ── HoT heal (fused atomic, tick context only) ───────────────────────
        Event::HotHealed {
            target,
            source_status,
            amount,
        } => {
            // no-op: HotHealed not produced during Cast or Move actions
            if ctx.cast.is_none() && ctx.move_.is_none() {
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    ctx.log.push(CombatEvent::HotHealed {
                        target: tgt_ent,
                        source_status: source_status.clone(),
                        amount: *amount,
                    });
                }
            }
        }

        // ── Zero-damage status tick ───────────────────────────────────────────
        Event::StatusTicked { .. } => {
            // no-op: zero-damage ticks have no CombatLog entry in any context
        }

        // ── Status changes ────────────────────────────────────────────────────
        Event::StatusRemoved { target, status } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusExpired {
                    target: tgt_ent,
                    status: status.clone(),
                });
            }
        }
        Event::StatusApplied { target, status } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    ctx.log.push(CombatEvent::StatusApplied {
                        target: tgt_ent,
                        status: status.clone(),
                    });
                }
            }
        }

        // ── Aura events (turn/round-boundary, any context) ────────────────────
        Event::AuraStatusGained {
            target, status_id, ..
        } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusApplied {
                    target: tgt_ent,
                    status: status_id.clone(),
                });
            }
        }
        Event::AuraStatusLost {
            target, status_id, ..
        } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusExpired {
                    target: tgt_ent,
                    status: status_id.clone(),
                });
            }
        }

        // ── Death ─────────────────────────────────────────────────────────────
        Event::UnitDied { unit } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::UnitDied { entity: ent });
                ctx.queues.deaths.push(*unit);
            }
        }

        // ── Healing (cast only) ───────────────────────────────────────────────
        Event::UnitHealed { target, amount } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
                ctx.log.push(CombatEvent::HealResult {
                    target: tgt_ent,
                    amount: *amount,
                });
            }
        }

        // ── Resource changes (C6: only PoolChanged remains) ──────────────────

        // ── Crit-fail (cast only) ─────────────────────────────────────────────
        Event::CritFailed {
            actor: actor_uid,
            outcome,
        } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(actor_ent) = ctx.id_map.get_entity(*actor_uid) else {
                    return;
                };
                match outcome {
                    combat_engine::CritFailOutcome::Miss => {
                        ctx.log.push(CombatEvent::CriticalMiss { actor: actor_ent });
                    }
                    _ => {
                        ctx.log.push(CombatEvent::CritFailSideEffect {
                            actor: actor_ent,
                            outcome: CritFailOutcomeEcs::from(outcome),
                        });
                    }
                }
            }
        }

        // ── Spawn / despawn (cast only) ───────────────────────────────────────
        Event::UnitSpawned { .. } => {
            // no-op in translate_one: UnitSpawned requires &mut Commands which
            // cannot be stored in TranslateCtx without propagating Bevy's system-
            // scoped Commands lifetime through the borrow graph.  Instead, callers
            // in cast context handle UnitSpawned in a separate post-pass after
            // translate_events returns (same pattern as PhaseEntered).
        }
        Event::SpawnBlocked {
            summoner: summoner_uid,
            reason,
            ..
        } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(summoner_entity) = ctx.id_map.get_entity(*summoner_uid) else {
                    return;
                };
                ctx.log.push(CombatEvent::SummonBlocked {
                    summoner: summoner_entity,
                    reason: SpawnBlockedReasonEcs::from(reason),
                });
            }
        }

        // ── Turn / round lifecycle (any context after B5) ─────────────────────
        Event::TurnEnded { actor, cause } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.queues.turn_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnEnded {
                    actor: ent,
                    cause: crate::game::combat_log::TurnEndCauseEcs::from(cause),
                });
            }
        }
        Event::TurnSkipped { actor, reason } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.queues.turn_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnSkipped {
                    actor: ent,
                    reason: TurnSkipReasonEcs::from(reason),
                });
            }
        }
        Event::RoundStarted { round } => {
            ctx.log.push(CombatEvent::RoundStarted { round: *round });
            ctx.queues.turn_lifecycle.round_started = true;
        }
        Event::TurnStarted { actor } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                // Always queue insert_active — the engine is the sole authority
                // for whose turn it is. Works uniformly for:
                //   round 1: settle_round_start (bootstrap)
                //   round 2+: BumpRound cascade
                //   mid-round: normal EndTurn handoff
                ctx.queues.turn_lifecycle.insert_active.push(*actor);
                ctx.log.push(CombatEvent::TurnStarted { actor: ent });
            }
        }

        // ── Action bookkeeping ────────────────────────────────────────────────
        Event::ActionStarted { .. } => {
            // no-op: action bookkeeping events have no CombatLog entry
        }
        Event::ActionFinished { .. } => {
            // no-op: action bookkeeping events have no CombatLog entry
        }

        // ── Phase transitions (handled at caller level) ───────────────────────
        Event::PhaseEntered { .. } => {
            // no-op: ECS writes for phase transitions are handled at the callsite
            // via pending_phases.0.push(...) after the translate_events call
        }

        // ── Unified pool-change (C6: sole pool-mutation event) ───────────────
        Event::PoolChanged {
            unit,
            pool,
            current,
            max,
            cause,
        } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::PoolChanged {
                    actor: ent,
                    pool: *pool,
                    current: *current,
                    max: *max,
                    cause: *cause,
                });
            }
        }

        // ── Hazard / env events ────────────────────────────────────────────────
        // A trap fired (one-shot) and was removed from the board — log the hit.
        Event::HazardTriggered { victim, .. } => {
            if let Some(victim_ent) = ctx.id_map.get_entity(*victim) {
                ctx.log
                    .push(CombatEvent::HazardTriggered { victim: victim_ent });
            }
        }
        // EnvRevealed: an armed trap became visible (reveal mechanic). Flag the
        // bridge so post-projection drains it into UiDirty. Not emitted on fire.
        Event::EnvRevealed { .. } => {
            ctx.queues.env_revealed = true;
        }

        // ── Initiative rolls (round-start, dormant until Wave 5) ─────────────
        // Emitted by CombatState::roll_initiative_for_all. Not wired into the
        // round lifecycle yet; translate here so the workspace compiles and the
        // combat-log rendering path is exercised once Wave 5 emits these.
        Event::InitiativeRolled {
            unit,
            roll,
            dex_mod,
            total,
        } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::InitiativeRolled {
                    actor: ent,
                    dex_mod: *dex_mod,
                    roll: *roll,
                    total: *total,
                });
            }
        }
    }
}

/// Emit the `CombatEvent::AbilityUsed` preamble for a cast action.
/// Called once before `translate_events` in the cast flow.
pub(crate) fn emit_ability_used(
    actor: Entity,
    ability: &combat_engine::AbilityId,
    target: Entity,
    target_pos: hexx::Hex,
    active_content: &ActiveContent,
    log: &mut CombatLog,
) {
    let (ability_name, is_aoe, cost_str) = active_content
        .abilities
        .get(ability)
        .map(|def| {
            let is_aoe = !matches!(def.aoe, crate::content::abilities::AoEShape::None);
            (def.name.clone(), is_aoe, format!("AP={}", def.cost_ap))
        })
        .unwrap_or_else(|| (ability.0.clone(), false, String::new()));

    log.push(CombatEvent::AbilityUsed {
        actor,
        ability_name,
        target,
        target_pos,
        is_aoe,
        cost_str,
    });
}
