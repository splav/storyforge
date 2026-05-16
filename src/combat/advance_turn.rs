#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::ActiveContent;
use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::game::components::{
    ActiveCombatant, Combatant, Dead, Faction, Team, Vital, VictoryTarget,
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
    let mut wrapped = false;

    {
        let prev = queue.index;
        queue.advance();
        if queue.index < prev {
            wrapped = true;
        }
    }

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
        let prev = queue.index;
        queue.advance();
        if queue.index < prev {
            wrapped = true;
        }
        if queue.index == start_idx {
            break;
        }
    }

    // 3. Hand off to the next actor or start a new round.
    for e in &active_q { commands.entity(e).remove::<ActiveCombatant>(); }

    if wrapped {
        next_phase.set(CombatPhase::StartRound);
    } else if let Some(next_actor) = queue.current() {
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

    // ── wrap-detection logic ────────────────────────────────────────────────
    //
    // Mirrors the inline loop in `advance_turn_system` so we can test the
    // wrap-detection without spinning up a full Bevy App.

    fn make_queue(len: usize, index: usize) -> TurnQueue {
        TurnQueue {
            order: (0..len as u32).map(|i| Entity::from_raw_u32(i).unwrap()).collect(),
            index,
        }
    }

    /// Simulates the advance loop from `advance_turn_system` using an index-based
    /// `is_alive` predicate. Returns `(wrapped, final_index)`.
    fn sim_advance(queue: &mut TurnQueue, is_alive: impl Fn(usize) -> bool) -> (bool, usize) {
        let start_idx = queue.index;
        let mut wrapped = false;

        let prev = queue.index;
        queue.advance();
        if queue.index < prev { wrapped = true; }

        loop {
            if is_alive(queue.index) { break; }
            let prev = queue.index;
            queue.advance();
            if queue.index < prev { wrapped = true; }
            if queue.index == start_idx { break; }
        }

        (wrapped, queue.index)
    }

    /// Normal mid-round handoff: index 1 → 2 (alive), no wrap.
    #[test]
    fn advance_mid_round_no_wrap() {
        let mut q = make_queue(3, 1);
        let (wrapped, next) = sim_advance(&mut q, |idx| idx == 2);
        assert!(!wrapped);
        assert_eq!(next, 2);
    }

    /// End-of-round: last slot (2) → wrap to 0 (alive) → StartRound.
    #[test]
    fn advance_end_of_round_wraps() {
        let mut q = make_queue(3, 2);
        let (wrapped, _) = sim_advance(&mut q, |idx| idx == 0);
        assert!(wrapped);
    }

    /// Dead unit at index 0 (Morok summoned, dies each round):
    /// slot 2 → wrap to 0 (dead, wrap detected!) → skip → 1 (alive).
    #[test]
    fn advance_dead_at_zero_after_wrap_still_detects_wrap() {
        let mut q = make_queue(3, 2);
        let (wrapped, next) = sim_advance(&mut q, |idx| idx == 1);
        assert!(wrapped, "wrap must be detected even when slot 0 is dead");
        assert_eq!(next, 1);
    }

    /// All dead: loop exhausts queue back to start_idx, wrapped=true (passed 2→0).
    #[test]
    fn advance_all_dead_wraps_back_to_start() {
        let mut q = make_queue(3, 1);
        let (wrapped, _) = sim_advance(&mut q, |_| false);
        assert!(wrapped);
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
