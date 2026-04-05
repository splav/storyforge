use bevy::prelude::*;
use crate::app_state::CombatPhase;
use crate::game::components::{ActionPoints, ActiveStatus, Combatant, Dead, Faction, StatusEffects, Team, Vital};
use crate::game::messages::{ApplyDamage, ApplyStatus, EndTurn};
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, GameDb, TurnQueue};

pub fn cleanup_system(
    mut commands: Commands,
    mut dmg_events: MessageReader<ApplyDamage>,
    mut status_events: MessageReader<ApplyStatus>,
    mut end_turn_events: MessageReader<EndTurn>,
    mut queries: ParamSet<(
        Query<&mut Vital>,
        Query<(&Vital, &Faction), With<Combatant>>,
    )>,
    mut action_points: Query<&mut ActionPoints>,
    mut statuses: Query<&mut StatusEffects>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    db: Res<GameDb>,
) {
    let damages: Vec<(Entity, i32)> =
        dmg_events.read().map(|e| (e.target, e.amount)).collect();
    let status_apps: Vec<(Entity, crate::core::StatusId, u32)> =
        status_events.read().map(|e| (e.target, e.status, e.duration_rounds)).collect();
    let end_turns: Vec<Entity> =
        end_turn_events.read().map(|e| e.actor).collect();

    // Apply damage with armor + defending mitigation; mark dead units.
    {
        let mut vitals = queries.p0();
        for (target, raw) in &damages {
            let Ok(mut v) = vitals.get_mut(*target) else { continue };

            let defending_bonus = statuses
                .get(*target)
                .map(|se| {
                    se.0.iter()
                        .filter_map(|s| db.statuses.get(&s.id))
                        .map(|def| def.armor_bonus)
                        .sum::<i32>()
                })
                .unwrap_or(0);

            let mitigated = (raw - v.armor - defending_bonus).max(1);
            v.apply_damage(mitigated);

            if !v.is_alive() {
                commands.entity(*target).insert(Dead);
                log.push(CombatEvent::UnitDied { entity: *target });
            }
        }
    }

    // Apply status effects.
    for (target, status, duration) in &status_apps {
        if let Ok(mut se) = statuses.get_mut(*target) {
            se.0.retain(|s| s.id != *status);
            se.0.push(ActiveStatus { id: *status, rounds_remaining: *duration });
        }
    }

    // Process end-of-turn: check win/lose, advance turn.
    for actor in end_turns {
        log.push(CombatEvent::TurnEnded { actor });
        tick_status_durations(actor, &mut statuses);

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

        // Advance to next living combatant.
        let start_idx = queue.index;
        queue.advance();
        loop {
            let alive = queue.current()
                .and_then(|e| queries.p0().get(e).ok().map(|v| v.is_alive()))
                .unwrap_or(false);
            if alive { break; }
            queue.advance();
            if queue.index == start_idx { break; }
        }

        if queue.index == 0 {
            next_phase.set(CombatPhase::StartRound);
        } else if let Some(next_actor) = queue.current() {
            // Reset action points for the new actor.
            if let Ok(mut ap) = action_points.get_mut(next_actor) {
                ap.action = true;
            }
            ctx.active = Some(next_actor);
            log.push(CombatEvent::TurnStarted { actor: next_actor });
        }
    }
}

fn tick_status_durations(actor: Entity, statuses: &mut Query<&mut StatusEffects>) {
    if let Ok(mut se) = statuses.get_mut(actor) {
        for s in se.0.iter_mut() {
            s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
        }
        se.0.retain(|s| s.rounds_remaining > 0);
    }
}
