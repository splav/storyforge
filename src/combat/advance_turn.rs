#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::core::DiceRng;
use crate::game::components::{
    ActionPoints, ActiveCombatant, ActiveStatus, Combatant, Dead, Faction, StatusEffects, Team, Vital,
};
use crate::game::messages::{ApplyStatus, EndTurn};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{GameDb, TurnQueue};
use bevy::prelude::*;

/// Consumes EndTurn and ApplyStatus messages.
/// Ticks existing statuses FIRST, then applies new ones (no +1 hack needed).
/// Checks win/lose, advances the turn queue.
pub fn advance_turn_system(
    mut commands: Commands,
    mut end_turn_events: MessageReader<EndTurn>,
    mut status_events: MessageReader<ApplyStatus>,
    mut queries: ParamSet<(
        Query<&mut Vital>,
        Query<(&Vital, &Faction), With<Combatant>>,
    )>,
    mut action_points: Query<&mut ActionPoints>,
    mut statuses: Query<(Entity, &mut StatusEffects)>,
    dead_q: Query<(), With<Dead>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut queue: ResMut<TurnQueue>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
) {
    // Consume all EndTurn messages (prevents leaking to next frame).
    // At most one actor ends their turn per frame, so take the first.
    let all_actors: Vec<Entity> = end_turn_events.read().map(|e| e.actor).collect();
    let Some(&actor) = all_actors.first() else { return };
    let status_apps: Vec<(Entity, Entity, crate::core::StatusId, u32)> = status_events
        .read()
        .map(|e| (e.source, e.target, e.status.clone(), e.duration_rounds))
        .collect();

    log.push(CombatEvent::TurnEnded { actor });

    // 1. Tick EXISTING statuses applied by this actor (before new ones are added).
    let tick_results = tick_status_durations(actor, &mut statuses, &db);
    // Apply DoT damage and log expired statuses.
    {
        let mut vitals = queries.p0();
        for result in &tick_results {
            match result {
                TickResult::DotDamage { target, damage, status } => {
                    if let Ok(mut v) = vitals.get_mut(*target) {
                        v.apply_damage(*damage);
                        log.push(CombatEvent::PoisonTick {
                            target: *target,
                            status: status.clone(),
                            damage: *damage,
                        });
                        if !v.is_alive() {
                            commands.entity(*target).insert(Dead);
                            log.push(CombatEvent::UnitDied { entity: *target });
                        }
                    }
                }
                TickResult::PercentDot { target, percent, status } => {
                    if let Ok(mut v) = vitals.get_mut(*target) {
                        let damage = (v.max_hp * *percent + 99) / 100;
                        v.apply_damage(damage);
                        log.push(CombatEvent::PoisonTick {
                            target: *target,
                            status: status.clone(),
                            damage,
                        });
                        if !v.is_alive() {
                            commands.entity(*target).insert(Dead);
                            log.push(CombatEvent::UnitDied { entity: *target });
                        }
                    }
                }
                TickResult::Expired { target, status } => {
                    log.push(CombatEvent::StatusExpired { target: *target, status: status.clone() });
                }
            }
        }
    }

    // 2. Apply NEW statuses (skip dead targets).
    for (source, target, status, duration) in &status_apps {
        if dead_q.get(*target).is_ok() {
            continue;
        }
        if let Ok((_, mut se)) = statuses.get_mut(*target) {
            se.0.retain(|s| s.id != *status);
            let dot_per_tick = db.statuses.get(status)
                .and_then(|sd| sd.dot_dice.as_ref())
                .map(|dice| rng.roll_dice(dice).0)
                .unwrap_or(0);
            se.0.push(ActiveStatus {
                id: status.clone(),
                rounds_remaining: *duration,
                applier: *source,
                dot_per_tick,
            });
        }
    }

    // 3. Win/lose check.
    if let Some(outcome) = check_combat_end(&queries.p1()) {
        end_combat(outcome, &mut log, &mut next_phase);
        return;
    }

    // 4. Advance to next living combatant.
    // Dead entities are skipped, but we still tick their orphaned statuses
    // so that effects they applied continue to expire on schedule.
    let start_idx = queue.index;
    queue.advance();
    loop {
        let current = queue.current();
        let alive = current
            .and_then(|e| queries.p0().get(e).ok().map(|v| v.is_alive()))
            .unwrap_or(false);
        if alive {
            break;
        }
        if let Some(dead_entity) = current {
            let tick_results = tick_status_durations(dead_entity, &mut statuses, &db);
            let mut vitals = queries.p0();
            for result in &tick_results {
                match result {
                    TickResult::DotDamage { target, damage, status } => {
                        if let Ok(mut v) = vitals.get_mut(*target) {
                            v.apply_damage(*damage);
                            log.push(CombatEvent::PoisonTick {
                                target: *target,
                                status: status.clone(),
                                damage: *damage,
                            });
                            if !v.is_alive() {
                                commands.entity(*target).insert(Dead);
                                log.push(CombatEvent::UnitDied { entity: *target });
                            }
                        }
                    }
                    TickResult::PercentDot { target, percent, status } => {
                        if let Ok(mut v) = vitals.get_mut(*target) {
                            let damage = (v.max_hp * *percent + 99) / 100;
                            v.apply_damage(damage);
                            log.push(CombatEvent::PoisonTick {
                                target: *target,
                                status: status.clone(),
                                damage,
                            });
                            if !v.is_alive() {
                                commands.entity(*target).insert(Dead);
                                log.push(CombatEvent::UnitDied { entity: *target });
                            }
                        }
                    }
                    TickResult::Expired { target, status } => {
                        log.push(CombatEvent::StatusExpired { target: *target, status: status.clone() });
                    }
                }
            }
        }
        queue.advance();
        if queue.index == start_idx {
            break;
        }
    }

    // 5. Recheck win/lose — DoT during the advance loop may have killed
    //    the last remaining player or enemy.
    if let Some(outcome) = check_combat_end(&queries.p1()) {
        end_combat(outcome, &mut log, &mut next_phase);
        return;
    }

    // 6. Hand off to the next actor or start a new round.
    for e in &active_q { commands.entity(e).remove::<ActiveCombatant>(); }

    if queue.index == 0 {
        next_phase.set(CombatPhase::StartRound);
    } else if let Some(next_actor) = queue.current() {
        if let Ok(mut ap) = action_points.get_mut(next_actor) {
            ap.action = true;
            ap.movement = true;
        }
        commands.entity(next_actor).insert(ActiveCombatant);
        log.push(CombatEvent::TurnStarted { actor: next_actor });
    }
}

/// Returns `Some(true)` for victory, `Some(false)` for defeat, `None` if combat continues.
fn check_combat_end(
    combatants: &Query<(&Vital, &Faction), With<Combatant>>,
) -> Option<bool> {
    let players_alive = combatants.iter().any(|(v, f)| v.is_alive() && f.0 == Team::Player);
    let enemies_alive = combatants.iter().any(|(v, f)| v.is_alive() && f.0 == Team::Enemy);

    if !enemies_alive {
        Some(true)
    } else if !players_alive {
        Some(false)
    } else {
        None
    }
}

fn end_combat(
    victory: bool,
    log: &mut CombatLog,
    next_phase: &mut NextState<CombatPhase>,
) {
    log.push(CombatEvent::CombatEnded { victory });
    next_phase.set(if victory { CombatPhase::Victory } else { CombatPhase::Defeat });
}

enum TickResult {
    DotDamage { target: Entity, damage: i32, status: crate::core::StatusId },
    PercentDot { target: Entity, percent: i32, status: crate::core::StatusId },
    Expired { target: Entity, status: crate::core::StatusId },
}

/// Ticks all statuses applied by `actor` across every entity.
/// Returns DoT damage events and expired status events.
fn tick_status_durations(
    actor: Entity,
    statuses: &mut Query<(Entity, &mut StatusEffects)>,
    db: &GameDb,
) -> Vec<TickResult> {
    let mut results = Vec::new();
    for (target, mut se) in statuses.iter_mut() {
        for s in se.0.iter_mut() {
            if s.applier != actor {
                continue;
            }
            // Apply DoT damage before decrementing.
            if s.dot_per_tick > 0 {
                results.push(TickResult::DotDamage {
                    target,
                    damage: s.dot_per_tick,
                    status: s.id.clone(),
                });
            }
            // Percentage-based DoT (heritage exhaustion).
            if let Some(sd) = db.statuses.get(&s.id) {
                if sd.hp_percent_dot > 0 {
                    results.push(TickResult::PercentDot {
                        target,
                        percent: sd.hp_percent_dot,
                        status: s.id.clone(),
                    });
                }
            }
            s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
        }
        let newly_expired: Vec<_> =
            se.0.iter()
                .filter(|s| s.applier == actor && s.rounds_remaining == 0)
                .map(|s| TickResult::Expired { target, status: s.id.clone() })
                .collect();
        results.extend(newly_expired);
        se.0.retain(|s| !(s.applier == actor && s.rounds_remaining == 0));
    }
    results
}
