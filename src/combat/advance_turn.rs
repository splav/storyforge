use crate::app_state::CombatPhase;
use crate::game::components::{
    ActionPoints, ActiveCombatant, ActiveStatus, Combatant, Dead, Faction, StatusEffects, Team, Vital,
};
use crate::game::messages::{ApplyStatus, EndTurn};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatContext, TurnQueue};
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
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    let end_turns: Vec<Entity> = end_turn_events.read().map(|e| e.actor).collect();
    let status_apps: Vec<(Entity, Entity, crate::core::StatusId, u32)> = status_events
        .read()
        .map(|e| (e.source, e.target, e.status.clone(), e.duration_rounds))
        .collect();

    for actor in end_turns {
        log.push(CombatEvent::TurnEnded { actor });

        // 1. Tick EXISTING statuses applied by this actor (before new ones are added).
        let expired = tick_status_durations(actor, &mut statuses);
        for (target, status) in expired {
            log.push(CombatEvent::StatusExpired { target, status });
        }

        // 2. Apply NEW statuses (skip dead targets).
        for (source, target, status, duration) in &status_apps {
            if dead_q.get(*target).is_ok() {
                continue;
            }
            if let Ok((_, mut se)) = statuses.get_mut(*target) {
                se.0.retain(|s| s.id != *status);
                se.0.push(ActiveStatus {
                    id: status.clone(),
                    rounds_remaining: *duration,
                    applier: *source,
                });
            }
        }

        // 3. Win/lose check.
        let (players_alive, enemies_alive) = {
            let q = queries.p1();
            let pa = q.iter().any(|(v, f)| v.is_alive() && f.0 == Team::Player);
            let ea = q.iter().any(|(v, f)| v.is_alive() && f.0 == Team::Enemy);
            (pa, ea)
        };

        if !enemies_alive {
            log.push(CombatEvent::CombatEnded { victory: true });
            next_phase.set(CombatPhase::Victory);
            return;
        }
        if !players_alive {
            log.push(CombatEvent::CombatEnded { victory: false });
            next_phase.set(CombatPhase::Defeat);
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
                let expired = tick_status_durations(dead_entity, &mut statuses);
                for (target, status) in expired {
                    log.push(CombatEvent::StatusExpired { target, status });
                }
            }
            queue.advance();
            if queue.index == start_idx {
                break;
            }
        }

        ctx.turn_ending = false;
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
}

/// Ticks all statuses applied by `actor` across every entity.
/// Removes expired statuses and returns (target, status_id) for each.
fn tick_status_durations(
    actor: Entity,
    statuses: &mut Query<(Entity, &mut StatusEffects)>,
) -> Vec<(Entity, crate::core::StatusId)> {
    let mut expired = Vec::new();
    for (target, mut se) in statuses.iter_mut() {
        for s in se.0.iter_mut() {
            if s.applier == actor {
                s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
            }
        }
        let newly_expired: Vec<_> =
            se.0.iter()
                .filter(|s| s.applier == actor && s.rounds_remaining == 0)
                .map(|s| (target, s.id.clone()))
                .collect();
        expired.extend(newly_expired);
        se.0.retain(|s| !(s.applier == actor && s.rounds_remaining == 0));
    }
    expired
}
