use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::game::components::{Combatant, Dead, Faction, Team, Vital, VictoryTarget};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatContext, CombatObjective, PhaseDeadline};
use bevy::prelude::*;

/// Event-driven victory/defeat detection. Runs after any system that may
/// insert `Dead`: checks the objective whenever at least one entity became
/// dead since the last run. Entities revived by `phase_transition_system`
/// (which removes `Dead`) no longer match `Added<Dead>` → no false positive.
pub fn check_victory_system(
    added_dead: Query<(), Added<Dead>>,
    combatants: Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    named_vitals: Query<(&Name, &Vital)>,
    objective: Res<CombatObjective>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    if added_dead.is_empty() {
        return;
    }
    if let Some(outcome) = check_combat_end(&combatants, &named_vitals, &objective.0) {
        end_combat(outcome, &mut log, &mut next_phase);
    }
}

/// Single-pass iteration over combatants → dispatch to `determine_outcome`.
///
/// `named_vitals` supplies `(Name, Vital)` pairs for `KeepAlive` lookups.
fn check_combat_end(
    combatants: &Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    named_vitals: &Query<(&Name, &Vital)>,
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

    let is_named_alive = |name: &str| -> bool {
        named_vitals
            .iter()
            .any(|(n, v)| n.as_str() == name && v.is_alive())
    };

    determine_outcome(players_alive, enemies_alive, target_alive, objective, &is_named_alive)
}

/// Pure victory/defeat decision. `Some(true)` = victory, `Some(false)` = defeat,
/// `None` = combat continues. Party wipe always beats objective progress.
///
/// `is_named_alive(name)` is called by `KeepAlive` to check if a specific named
/// unit is still alive.  Pass a closure that looks up ECS `Name` + `Vital`.
pub(crate) fn determine_outcome(
    players_alive: bool,
    enemies_alive: bool,
    target_alive: bool,
    objective: &VictoryCondition,
    is_named_alive: &dyn Fn(&str) -> bool,
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
        VictoryCondition::KeepAlive { target_name, .. } => {
            if !is_named_alive(target_name) {
                Some(false) // protected unit died → immediate defeat
            } else if !enemies_alive {
                Some(true) // target survived and no enemies remain → condition satisfied
            } else {
                None // combat ongoing, target alive — continue
            }
        }
        VictoryCondition::AllOf(conditions) => {
            let mut all_victory = true;
            for cond in conditions {
                match determine_outcome(
                    players_alive,
                    enemies_alive,
                    target_alive,
                    cond,
                    is_named_alive,
                ) {
                    Some(false) => return Some(false), // short-circuit on any defeat
                    Some(true) => {}                   // this sub-condition satisfied
                    None => all_victory = false,       // still in progress
                }
            }
            if all_victory { Some(true) } else { None }
        }
    }
}

/// Positive predicate: did `cond` hold in the FINAL combat state?
///
/// Unlike `determine_outcome`, this carries NO "KeepAlive-death → defeat"
/// semantics — it simply answers "was this (possibly secondary) objective
/// achieved?", for recording campaign flags at combat end. Pure; ECS-free.
pub(crate) fn objective_met(
    cond: &VictoryCondition,
    enemies_alive: bool,
    is_named_alive: &dyn Fn(&str) -> bool,
) -> bool {
    match cond {
        VictoryCondition::AllEnemiesDead => !enemies_alive,
        VictoryCondition::KillTarget { enemy_name, .. } => !is_named_alive(enemy_name),
        VictoryCondition::KeepAlive { target_name, .. } => is_named_alive(target_name),
        VictoryCondition::AllOf(conds) => {
            conds.iter().all(|c| objective_met(c, enemies_alive, is_named_alive))
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

/// Round-based phase deadline: once `limit` rounds have elapsed since a phase
/// override activated, resolve the current objective. If it is already a victory
/// (player killed the target in time) → victory; otherwise the boss escaped → defeat.
/// Not gated by `Added<Dead>` — must fire purely on round count.
pub fn check_phase_deadline_system(
    deadline: Res<PhaseDeadline>,
    ctx: Res<CombatContext>,
    combatants: Query<(&Vital, &Faction, Option<&VictoryTarget>), With<Combatant>>,
    named_vitals: Query<(&Name, &Vital)>,
    objective: Res<CombatObjective>,
    mut log: ResMut<CombatLog>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    let Some(state) = &deadline.0 else { return };
    if ctx.round.saturating_sub(state.phase_started_round) < state.limit {
        return;
    }
    let victory = matches!(
        check_combat_end(&combatants, &named_vitals, &objective.0),
        Some(true)
    );
    end_combat(victory, &mut log, &mut next_phase);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default `is_named_alive` stub — all named units alive (conservative default).
    fn always_alive(_name: &str) -> bool { true }
    /// Stub that returns false for every name (no named unit alive).
    fn never_alive(_name: &str) -> bool { false }

    // ── determine_outcome ───────────────────────────────────────────────

    #[test]
    fn outcome_defeat_when_no_players_alive() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(false, true, true, &obj, &always_alive), Some(false));
    }

    #[test]
    fn outcome_all_enemies_dead_victory() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(true, false, false, &obj, &always_alive), Some(true));
    }

    #[test]
    fn outcome_all_enemies_dead_continues_while_enemies_alive() {
        let obj = VictoryCondition::AllEnemiesDead;
        assert_eq!(determine_outcome(true, true, false, &obj, &always_alive), None);
    }

    #[test]
    fn outcome_kill_target_victory_on_target_dead() {
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        // Enemies still alive but target down → victory.
        assert_eq!(determine_outcome(true, true, false, &obj, &always_alive), Some(true));
    }

    #[test]
    fn outcome_kill_target_continues_while_target_alive() {
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert_eq!(determine_outcome(true, false, true, &obj, &always_alive), None);
    }

    #[test]
    fn outcome_party_wipe_beats_objective_completion() {
        // Even if target is dead, a party wipe is a defeat, not a victory.
        let obj = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert_eq!(determine_outcome(false, false, false, &obj, &always_alive), Some(false));
    }

    // ── NonActingNpc does not count as player ────────────────────────────

    /// A party with ONLY a living NPC and no active players → defeat.
    /// This models check_combat_end skipping NPC → players_alive stays false.
    #[test]
    fn outcome_party_with_only_npc_alive_counts_as_defeat() {
        let obj = VictoryCondition::AllEnemiesDead;
        // NPC is alive but filtered out → players_alive = false, enemies_alive = true
        // This is defeat (party wipe from engine's perspective).
        assert_eq!(determine_outcome(false, true, false, &obj, &always_alive), Some(false));
    }

    /// A living NPC must not satisfy "enemies alive" for AllEnemiesDead objective.
    /// NPCs are on Team::Player, so they can never be in enemies_alive anyway.
    /// This test documents that: enemies_alive=false + players_alive=true → victory.
    #[test]
    fn outcome_npc_does_not_satisfy_enemies_alive_for_all_enemies_dead() {
        let obj = VictoryCondition::AllEnemiesDead;
        // If only NPC and hero alive (npc filtered out of enemies count),
        // enemies_alive = false → victory.
        assert_eq!(determine_outcome(true, false, false, &obj, &always_alive), Some(true));
    }

    // ── KeepAlive ────────────────────────────────────────────────────────

    #[test]
    fn outcome_keep_alive_target_dead_is_defeat() {
        let obj = VictoryCondition::KeepAlive {
            target_name: "Магистр".into(),
            marker_color: [0.3, 0.6, 1.0],
        };
        // Target dead → immediate defeat regardless of combat state.
        assert_eq!(determine_outcome(true, true, false, &obj, &never_alive), Some(false));
    }

    #[test]
    fn outcome_keep_alive_target_alive_with_enemies_alive_is_none() {
        let obj = VictoryCondition::KeepAlive {
            target_name: "Магистр".into(),
            marker_color: [0.3, 0.6, 1.0],
        };
        // Target alive but enemies remain → leaf cannot win alone → None.
        assert_eq!(determine_outcome(true, true, false, &obj, &always_alive), None);
    }

    #[test]
    fn outcome_keep_alive_target_alive_no_enemies_is_victory() {
        // KeepAlive standalone: target alive + no enemies → victory.
        let obj = VictoryCondition::KeepAlive {
            target_name: "Магистр".into(),
            marker_color: [0.3, 0.6, 1.0],
        };
        assert_eq!(determine_outcome(true, false, false, &obj, &always_alive), Some(true));
    }

    // ── AllOf ────────────────────────────────────────────────────────────

    #[test]
    fn outcome_allof_short_circuits_on_first_defeat() {
        // First sub-condition is a KeepAlive with dead target → defeat immediately.
        let obj = VictoryCondition::AllOf(vec![
            VictoryCondition::KeepAlive {
                target_name: "Магистр".into(),
                marker_color: [0.3, 0.6, 1.0],
            },
            VictoryCondition::AllEnemiesDead,
        ]);
        // Магистр is dead → short-circuit defeat.
        assert_eq!(determine_outcome(true, false, false, &obj, &never_alive), Some(false));
    }

    #[test]
    fn outcome_nested_allof_evaluates_recursively() {
        // AllOf([AllEnemiesDead, KeepAlive("Магистр")]) with all satisfied.
        let is_magistr_alive = |name: &str| name == "Магистр";
        let obj = VictoryCondition::AllOf(vec![
            VictoryCondition::AllEnemiesDead,
            VictoryCondition::KeepAlive {
                target_name: "Магистр".into(),
                marker_color: [0.3, 0.6, 1.0],
            },
        ]);
        // Enemies dead + Магистр alive → both satisfied → victory.
        assert_eq!(
            determine_outcome(true, false, false, &obj, &is_magistr_alive),
            Some(true)
        );
    }

    // ── objective_text ───────────────────────────────────────────────────

    #[test]
    fn objective_text_renders_allof_with_and_separator() {
        let obj = VictoryCondition::AllOf(vec![
            VictoryCondition::AllEnemiesDead,
            VictoryCondition::KeepAlive {
                target_name: "Магистр".into(),
                marker_color: [0.0, 0.0, 0.0],
            },
        ]);
        let text = obj.objective_text();
        assert!(text.contains(" и "), "AllOf text must use ' и ' separator: {text}");
        assert!(text.contains("Победить всех врагов"), "must include AllEnemiesDead text");
        assert!(text.contains("Магистр"), "must include KeepAlive target name");
    }

    // ── objective_met ────────────────────────────────────────────────────────

    #[test]
    fn objective_met_keep_alive_target_alive_is_true() {
        let cond = VictoryCondition::KeepAlive { target_name: "А".into(), marker_color: [0.0; 3] };
        assert!(objective_met(&cond, false, &always_alive));
    }

    #[test]
    fn objective_met_keep_alive_target_dead_is_false() {
        let cond = VictoryCondition::KeepAlive { target_name: "А".into(), marker_color: [0.0; 3] };
        assert!(!objective_met(&cond, false, &never_alive));
    }

    #[test]
    fn objective_met_all_enemies_dead_no_enemies_is_true() {
        assert!(objective_met(&VictoryCondition::AllEnemiesDead, false, &always_alive));
    }

    #[test]
    fn objective_met_all_enemies_dead_enemies_alive_is_false() {
        assert!(!objective_met(&VictoryCondition::AllEnemiesDead, true, &always_alive));
    }

    #[test]
    fn objective_met_kill_target_dead_is_true() {
        let cond = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert!(objective_met(&cond, false, &never_alive));
    }

    #[test]
    fn objective_met_kill_target_alive_is_false() {
        let cond = VictoryCondition::KillTarget {
            enemy_name: "boss".into(),
            marker_color: [1.0, 0.0, 0.0],
            description: None,
        };
        assert!(!objective_met(&cond, true, &always_alive));
    }

    #[test]
    fn objective_met_allof_both_satisfied_is_true() {
        let cond = VictoryCondition::AllOf(vec![
            VictoryCondition::KeepAlive { target_name: "А".into(), marker_color: [0.0; 3] },
            VictoryCondition::AllEnemiesDead,
        ]);
        // Target alive ("А" matched by always_alive) and no enemies.
        assert!(objective_met(&cond, false, &always_alive));
    }

    #[test]
    fn objective_met_allof_one_failing_is_false() {
        let cond = VictoryCondition::AllOf(vec![
            VictoryCondition::KeepAlive { target_name: "А".into(), marker_color: [0.0; 3] },
            VictoryCondition::AllEnemiesDead,
        ]);
        // Enemies still alive → AllEnemiesDead fails.
        assert!(!objective_met(&cond, true, &always_alive));
    }
}
