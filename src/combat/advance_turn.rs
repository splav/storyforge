#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::ActiveContent;
use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::game::components::{
    ActionPoints, ActiveCombatant, Combatant, Dead, Faction, Speed, StatusEffects, Team, Vital, VictoryTarget,
};
use crate::game::messages::EndTurn;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatObjective, TurnQueue};
use crate::combat::engine_bridge::{
    build_ecs_content_view, translate_tick_events, CombatStateRes, UnitIdMap,
};
use bevy::prelude::*;

/// Consumes EndTurn messages and advances the turn queue.
///
/// Statuses are now applied by `project_state_to_ecs` (engine projector) and
/// `apply_auras_system` (TurnStart). Victory/defeat detection lives in
/// `check_victory_system`, which is event-driven on `Added<Dead>` and runs
/// after this system.
///
/// DoT ticks for statuses applied by a combatant fire at that combatant's
/// next `TurnStart` (see `status_tick::tick_status_effects_system`), not here.
/// Это даёт `phase_transition_system` в `Execute` того же кадра возможность
/// оживить фазированного босса до victory-check.
pub fn advance_turn_system(
    mut commands: Commands,
    mut end_turn_events: MessageReader<EndTurn>,
    vitals: Query<&Vital>,
    mut action_points: Query<&mut ActionPoints>,
    speed_q: Query<&Speed>,
    statuses: Query<(Entity, &StatusEffects)>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut queue: ResMut<TurnQueue>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    content: Res<ActiveContent>,
    mut combat_state: ResMut<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    combatants: Query<crate::combat::engine_bridge::AooRow, With<Combatant>>,
) {
    // Note: ApplyStatus consumer removed in Phase 2 step 9d. Statuses are now
    // applied via project_state_to_ecs (engine projector) each frame.

    // Consume all EndTurn messages (prevents leaking to next frame).
    // At most one actor ends their turn per frame, so take the first.
    let all_actors: Vec<Entity> = end_turn_events.read().map(|e| e.actor).collect();
    let Some(&actor) = all_actors.first() else { return };

    log.push(CombatEvent::TurnEnded { actor });

    // 2. Advance to next living combatant.
    // Dead entities are skipped; we tick their orphaned statuses via the engine
    // so sirota-DoT effects continue to expire on schedule. Any deaths insert
    // `Dead`; `check_victory_system` picks them up downstream.
    let start_idx = queue.index;
    queue.advance();
    loop {
        let current = queue.current();
        let alive = current
            .and_then(|e| vitals.get(e).ok().map(|v| v.is_alive()))
            .unwrap_or(false);
        if alive {
            break;
        }
        if let Some(dead_entity) = current {
            if let Some(dead_uid) = id_map.get_id(dead_entity) {
                let view = build_ecs_content_view(&combatants, &id_map, &content);
                let events = combat_state.0.tick_actor_statuses(dead_uid, &view);
                translate_tick_events(&events, &id_map, &mut commands, &mut log);
            }
        }
        queue.advance();
        if queue.index == start_idx {
            break;
        }
    }

    // 3. Hand off to the next actor or start a new round.
    for e in &active_q { commands.entity(e).remove::<ActiveCombatant>(); }

    if queue.index == 0 {
        next_phase.set(CombatPhase::StartRound);
    } else if let Some(next_actor) = queue.current() {
        if let Ok(mut ap) = action_points.get_mut(next_actor) {
            let base = speed_q.get(next_actor).map(|s| s.0).unwrap_or(0);
            let next_statuses = statuses.get(next_actor).ok().map(|(_, s)| s);
            ap.movement_points =
                crate::combat::turn_order::refill_movement_points(base, next_statuses, &content);
        }
        commands.entity(next_actor).insert(ActiveCombatant);
        log.push(CombatEvent::TurnStarted { actor: next_actor });
    }
}

/// Event-driven victory/defeat detection. Runs after any system that may
/// insert `Dead`: checks the objective whenever at least one entity became
/// dead since the last run. Entities revived by `phase_transition_system`
/// (which removes `Dead`) no longer match `Added<Dead>` → no false positive.
pub fn check_victory_system(
    added_dead: Query<(), Added<Dead>>,
    combatants: Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    objective: Res<CombatObjective>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    if added_dead.is_empty() {
        return;
    }
    if let Some(outcome) = check_combat_end(&combatants, &objective.0) {
        end_combat(outcome, &mut log, &mut next_phase);
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

fn end_combat(
    victory: bool,
    log: &mut CombatLog,
    next_phase: &mut NextState<CombatPhase>,
) {
    log.push(CombatEvent::CombatEnded { victory });
    next_phase.set(if victory { CombatPhase::Victory } else { CombatPhase::Defeat });
}

#[cfg(test)]
mod tests {
    use super::*;

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

}
