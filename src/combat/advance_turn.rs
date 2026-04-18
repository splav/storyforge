#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::app_state::CombatPhase;
use crate::core::DiceRng;
use crate::content::encounters::VictoryCondition;
use crate::game::components::{
    ActionPoints, ActiveCombatant, ActiveStatus, Combatant, Dead, Faction, Speed, StatusEffects, Team, Vital, VictoryTarget,
};
use crate::core::StatusId;
use crate::game::messages::{ApplyStatus, EndTurn};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatObjective, TurnQueue};
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
        Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    )>,
    mut action_points: Query<&mut ActionPoints>,
    speed_q: Query<&Speed>,
    mut statuses: Query<(Entity, &mut StatusEffects)>,
    dead_q: Query<(), With<Dead>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut queue: ResMut<TurnQueue>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    content: Res<ActiveContent>,
    objective: Res<CombatObjective>,
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
    let tick_results = tick_status_durations(actor, &mut statuses, &content);
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
                        let damage = percent_dot_damage(v.max_hp, *percent);
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
            let dot_per_tick = content.statuses.get(status)
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
    if let Some(outcome) = check_combat_end(&queries.p1(), &objective.0) {
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
            let tick_results = tick_status_durations(dead_entity, &mut statuses, &content);
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
                            let damage = percent_dot_damage(v.max_hp, *percent);
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
    if let Some(outcome) = check_combat_end(&queries.p1(), &objective.0) {
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
            let base = speed_q.get(next_actor).map(|s| s.0).unwrap_or(0);
            let next_statuses = statuses.get(next_actor).ok().map(|(_, s)| s);
            ap.movement_points =
                crate::combat::turn_order::refill_movement_points(base, next_statuses, &content);
        }
        commands.entity(next_actor).insert(ActiveCombatant);
        log.push(CombatEvent::TurnStarted { actor: next_actor });
    }
}

/// Single-pass iteration over combatants → dispatch to `determine_outcome`.
fn check_combat_end(
    combatants: &Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    objective: &VictoryCondition,
) -> Option<bool> {
    let mut players_alive = false;
    let mut enemies_alive = false;
    let mut target_alive = false;
    for (v, f, tag) in combatants.iter() {
        if !v.is_alive() {
            continue;
        }
        match f.0 {
            Team::Player => players_alive = true,
            Team::Enemy => enemies_alive = true,
        }
        if tag.is_some() {
            target_alive = true;
        }
    }
    determine_outcome(players_alive, enemies_alive, target_alive, objective)
}

/// Pure victory/defeat decision. `Some(true)` = victory, `Some(false)` = defeat,
/// `None` = combat continues. Party wipe always beats objective progress.
pub(crate) fn determine_outcome(
    players_alive: bool,
    enemies_alive: bool,
    target_alive: bool,
    objective: &VictoryCondition,
) -> Option<bool> {
    if !players_alive {
        return Some(false);
    }
    match objective {
        VictoryCondition::AllEnemiesDead => {
            if enemies_alive { None } else { Some(true) }
        }
        VictoryCondition::KillTarget { .. } => {
            if target_alive { None } else { Some(true) }
        }
    }
}

/// Ceiling-divide percentage damage: at least 1 damage for any positive percent
/// (rounds up so 1% of a 20-HP unit still ticks for 1 instead of 0).
pub(crate) fn percent_dot_damage(max_hp: i32, percent: i32) -> i32 {
    (max_hp * percent + 99) / 100
}

fn end_combat(
    victory: bool,
    log: &mut CombatLog,
    next_phase: &mut NextState<CombatPhase>,
) {
    log.push(CombatEvent::CombatEnded { victory });
    next_phase.set(if victory { CombatPhase::Victory } else { CombatPhase::Defeat });
}

pub(crate) enum TickResult {
    DotDamage { target: Entity, damage: i32, status: StatusId },
    PercentDot { target: Entity, percent: i32, status: StatusId },
    Expired { target: Entity, status: StatusId },
}

/// Bevy wrapper: iterates all entities and delegates per-entity tick logic.
fn tick_status_durations(
    actor: Entity,
    statuses: &mut Query<(Entity, &mut StatusEffects)>,
    content: &ContentView,
) -> Vec<TickResult> {
    let mut results = Vec::new();
    for (target, mut se) in statuses.iter_mut() {
        results.extend(tick_statuses_on_entity(actor, target, &mut se.0, content));
    }
    results
}

/// Pure per-entity status tick: emit DoT events, decrement durations, drop expired.
/// Only affects statuses whose `applier == actor`. `effects` is mutated in place.
pub(crate) fn tick_statuses_on_entity(
    actor: Entity,
    target: Entity,
    effects: &mut Vec<ActiveStatus>,
    content: &ContentView,
) -> Vec<TickResult> {
    let mut results = Vec::new();
    for s in effects.iter_mut() {
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
        if let Some(sd) = content.statuses.get(&s.id) {
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
    let newly_expired: Vec<_> = effects
        .iter()
        .filter(|s| s.applier == actor && s.rounds_remaining == 0)
        .map(|s| TickResult::Expired { target, status: s.id.clone() })
        .collect();
    results.extend(newly_expired);
    effects.retain(|s| !(s.applier == actor && s.rounds_remaining == 0));
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    // ── percent_dot_damage ──────────────────────────────────────────────

    #[test]
    fn percent_dot_exact_division() {
        assert_eq!(percent_dot_damage(100, 10), 10);
        assert_eq!(percent_dot_damage(20, 25), 5);
    }

    #[test]
    fn percent_dot_ceils_remainder() {
        // 20 * 1% = 0.2 → 1 damage after ceil
        assert_eq!(percent_dot_damage(20, 1), 1);
        // 7 * 10% = 0.7 → 1 damage after ceil
        assert_eq!(percent_dot_damage(7, 10), 1);
    }

    #[test]
    fn percent_dot_zero_percent_gives_zero() {
        // Edge case: 0% should be 0, not 1 (callers guard with > 0 check anyway).
        assert_eq!(percent_dot_damage(100, 0), 0);
    }

    // ── determine_outcome ───────────────────────────────────────────────

    #[test]
    fn outcome_defeat_when_no_players_alive() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(false, true, true, &obj), Some(false));
    }

    #[test]
    fn outcome_all_enemies_dead_victory() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(true, false, false, &obj), Some(true));
    }

    #[test]
    fn outcome_all_enemies_dead_continues_while_enemies_alive() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(true, true, false, &obj), None);
    }

    #[test]
    fn outcome_kill_target_victory_on_target_dead() {
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        // Enemies still alive but target down → victory.
        assert_eq!(determine_outcome(true, true, false, &obj), Some(true));
    }

    #[test]
    fn outcome_kill_target_continues_while_target_alive() {
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert_eq!(determine_outcome(true, false, true, &obj), None);
    }

    #[test]
    fn outcome_party_wipe_beats_objective_completion() {
        // Even if target is dead, a party wipe is a defeat, not a victory.
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert_eq!(determine_outcome(false, false, false, &obj), Some(false));
    }

    // ── tick_statuses_on_entity ─────────────────────────────────────────

    fn active_status(id: &str, applier: Entity, rounds: u32, dot: i32) -> ActiveStatus {
        ActiveStatus {
            id: id.into(),
            rounds_remaining: rounds,
            applier,
            dot_per_tick: dot,
        }
    }

    #[test]
    fn tick_ignores_statuses_from_other_applier() {
        let actor = ent(1);
        let other = ent(2);
        let target = ent(3);
        let content = ContentView::load_global_for_tests();
        let mut effects = vec![active_status("burning", other, 2, 3)];

        let results = tick_statuses_on_entity(actor, target, &mut effects, &content);
        assert!(results.is_empty());
        // Untouched: still 2 rounds, still present.
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].rounds_remaining, 2);
    }

    #[test]
    fn tick_emits_dot_and_decrements() {
        let actor = ent(1);
        let target = ent(3);
        let content = ContentView::load_global_for_tests();
        let mut effects = vec![active_status("burning", actor, 3, 5)];

        let results = tick_statuses_on_entity(actor, target, &mut effects, &content);
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0],
            TickResult::DotDamage { damage: 5, .. }
        ));
        assert_eq!(effects[0].rounds_remaining, 2);
    }

    #[test]
    fn tick_emits_expired_and_removes_at_zero_rounds() {
        let actor = ent(1);
        let target = ent(3);
        let content = ContentView::load_global_for_tests();
        // 1 round remaining → after tick: 0 → expired.
        let mut effects = vec![active_status("stunned", actor, 1, 0)];

        let results = tick_statuses_on_entity(actor, target, &mut effects, &content);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], TickResult::Expired { .. }));
        assert!(effects.is_empty(), "expired status should be removed");
    }

    #[test]
    fn tick_emits_both_dot_and_expiration_in_same_call() {
        let actor = ent(1);
        let target = ent(3);
        let content = ContentView::load_global_for_tests();
        // dot + last round → both events from one status.
        let mut effects = vec![active_status("burning", actor, 1, 4)];

        let results = tick_statuses_on_entity(actor, target, &mut effects, &content);
        assert_eq!(results.len(), 2);
        assert!(matches!(&results[0], TickResult::DotDamage { damage: 4, .. }));
        assert!(matches!(&results[1], TickResult::Expired { .. }));
        assert!(effects.is_empty());
    }

    #[test]
    fn tick_only_affects_matching_applier_in_mixed_list() {
        let actor = ent(1);
        let other = ent(2);
        let target = ent(3);
        let content = ContentView::load_global_for_tests();
        let mut effects = vec![
            active_status("burning", actor, 2, 3),
            active_status("poisoned", other, 2, 4),
            active_status("stunned", actor, 1, 0),
        ];

        let results = tick_statuses_on_entity(actor, target, &mut effects, &content);
        // actor: burning → DotDamage; stunned → Expired.
        assert_eq!(results.len(), 2);
        // Other's status untouched.
        assert_eq!(effects.len(), 2, "other applier's status should remain");
        let poisoned = effects.iter().find(|s| s.id == "poisoned".into()).unwrap();
        assert_eq!(poisoned.rounds_remaining, 2);
    }
}
