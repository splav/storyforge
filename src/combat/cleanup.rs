use crate::app_state::CombatPhase;
use crate::game::components::{
    ActionPoints, ActiveStatus, Combatant, Dead, Faction, Mana, Rage, StatusEffects, Team, Vital,
};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn};
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, GameDb, TurnQueue};
use bevy::prelude::*;

pub fn cleanup_system(
    mut commands: Commands,
    mut dmg_events: MessageReader<ApplyDamage>,
    mut heal_events: MessageReader<ApplyHeal>,
    mut status_events: MessageReader<ApplyStatus>,
    mut end_turn_events: MessageReader<EndTurn>,
    mut queries: ParamSet<(
        Query<&mut Vital>,
        Query<(&Vital, &Faction), With<Combatant>>,
    )>,
    mut action_points: Query<&mut ActionPoints>,
    mut statuses: Query<(Entity, &mut StatusEffects)>,
    mut rage_query: Query<&mut Rage>,
    mut mana_query: Query<&mut Mana>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    db: Res<GameDb>,
) {
    let damages: Vec<(Entity, Entity, i32, String, bool)> = // (target, source, amount, breakdown, pierces_armor)
        dmg_events
            .read()
            .map(|e| (e.target, e.source, e.amount, e.breakdown.clone(), e.pierces_armor))
            .collect();
    let heals: Vec<(Entity, i32, String)> = heal_events
        .read()
        .map(|e| (e.target, e.amount, e.breakdown.clone()))
        .collect();
    let status_apps: Vec<(Entity, Entity, crate::core::StatusId, u32)> = // (source, target, status, duration)
        status_events
            .read()
            .map(|e| (e.source, e.target, e.status.clone(), e.duration_rounds))
            .collect();
    let end_turns: Vec<Entity> = end_turn_events.read().map(|e| e.actor).collect();

    // Apply damage with armor + defending mitigation; mark dead units.
    {
        let mut vitals = queries.p0();
        for (target, source, raw, formula, pierces_armor) in &damages {
            let Ok(mut v) = vitals.get_mut(*target) else {
                continue;
            };

            let status_sums = statuses
                .get(*target)
                .map(|(_, se)| {
                    se.0.iter().filter_map(|s| db.statuses.get(&s.id)).fold(
                        (0i32, 0i32),
                        |(armor, vuln), def| {
                            (armor + def.armor_bonus, vuln + def.damage_taken_bonus)
                        },
                    )
                })
                .unwrap_or((0, 0));

            let total_armor = if *pierces_armor {
                0
            } else {
                v.armor + status_sums.0
            };
            let vulnerability = status_sums.1;

            let final_damage = (raw - total_armor + vulnerability).max(1);
            v.apply_damage(final_damage);

            log.push(CombatEvent::DamageResult {
                target: *target,
                formula: formula.clone(),
                armor_reduced: total_armor,
                final_damage,
            });

            if !v.is_alive() {
                commands.entity(*target).insert(Dead);
                log.push(CombatEvent::UnitDied { entity: *target });
            }
        }

        // Apply heals (no armor reduction).
        for (target, amount, formula) in &heals {
            if let Ok(mut v) = vitals.get_mut(*target) {
                let before = v.hp;
                v.apply_heal(*amount);
                let actual = v.hp - before;
                log.push(CombatEvent::HealResult {
                    target: *target,
                    formula: formula.clone(),
                    amount: actual,
                });
            }
        }
    }

    // Rage: +1 for attacker (dealt damage) and defender (received damage).
    for (target, source, _, _, _) in &damages {
        for actor in [source, target] {
            if let Ok(mut rage) = rage_query.get_mut(*actor) {
                let current = rage.gain();
                log.push(CombatEvent::RageGained {
                    actor: *actor,
                    current,
                    max: rage.max,
                });
            }
        }
    }

    // Apply status effects.
    for (source, target, status, duration) in &status_apps {
        if let Ok((_, mut se)) = statuses.get_mut(*target) {
            se.0.retain(|s| s.id != *status);
            se.0.push(ActiveStatus {
                id: status.clone(),
                // +1: the tick at end of this same turn consumes one count,
                // so duration=1 effectively lasts until end of applier's NEXT turn.
                rounds_remaining: *duration + 1,
                applier: *source,
            });
        }
    }

    // Process end-of-turn: check win/lose, advance turn.
    for actor in end_turns {
        log.push(CombatEvent::TurnEnded { actor });

        // Tick statuses applied by this actor (on any target).
        let expired: Vec<(Entity, crate::core::StatusId)> = tick_status_durations(actor, &mut statuses);
        for (target, status) in expired {
            log.push(CombatEvent::StatusExpired { target, status });
        }

        // Mana regeneration: +1 per turn.
        if let Ok(mut mana) = mana_query.get_mut(actor) {
            if mana.current < mana.max {
                let current = mana.restore(1);
                log.push(CombatEvent::ManaChanged {
                    actor,
                    current,
                    max: mana.max,
                });
            }
        }

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
            let alive = queue
                .current()
                .and_then(|e| queries.p0().get(e).ok().map(|v| v.is_alive()))
                .unwrap_or(false);
            if alive {
                break;
            }
            queue.advance();
            if queue.index == start_idx {
                break;
            }
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

/// Ticks all statuses applied by `actor` across every entity.
/// Removes expired statuses and returns (target, status_id) for each.
fn tick_status_durations(
    actor: Entity,
    statuses: &mut Query<(Entity, &mut StatusEffects)>,
) -> Vec<(Entity, crate::core::StatusId)> {
    let mut expired = Vec::new();
    for (target, mut se) in statuses.iter_mut() {
        let before = se.0.len();
        for s in se.0.iter_mut() {
            if s.applier == actor {
                s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
            }
        }
        let newly_expired: Vec<_> = se
            .0
            .iter()
            .filter(|s| s.applier == actor && s.rounds_remaining == 0)
            .map(|s| (target, s.id.clone()))
            .collect();
        expired.extend(newly_expired);
        se.0.retain(|s| !(s.applier == actor && s.rounds_remaining == 0));
    }
    expired
}
