use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::game::components::{Combatant, Dead, Faction, NonActingNpc, Team, Vital, VictoryTarget};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::CombatObjective;
use bevy::prelude::*;

/// Event-driven victory/defeat detection. Runs after any system that may
/// insert `Dead`: checks the objective whenever at least one entity became
/// dead since the last run. Entities revived by `phase_transition_system`
/// (which removes `Dead`) no longer match `Added<Dead>` → no false positive.
pub fn check_victory_system(
    added_dead: Query<(), Added<Dead>>,
    combatants: Query<(&Vital, &Faction, Option<&VictoryTarget>, Option<&NonActingNpc>), With<Combatant>>,
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
/// `NonActingNpc` entities are excluded from both `players_alive` and
/// `enemies_alive` counts — they must not keep the party "alive" if all
/// acting players are dead, nor affect enemy-alive detection.
fn check_combat_end(
    combatants: &Query<(&Vital, &Faction, Option<&VictoryTarget>, Option<&NonActingNpc>), With<Combatant>>,
    objective: &VictoryCondition,
) -> Option<bool> {
    let mut players_alive = false;
    let mut enemies_alive = false;
    let mut target_alive = false;
    for (v, f, tag, npc) in combatants.iter() {
        // Non-acting NPCs are not counted toward living parties.
        if npc.is_some() {
            continue;
        }
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

    // ── NonActingNpc does not count as player ────────────────────────────

    /// A party with ONLY a living NPC and no active players → defeat.
    /// This models check_combat_end skipping NPC → players_alive stays false.
    #[test]
    fn outcome_party_with_only_npc_alive_counts_as_defeat() {
        let obj = VictoryCondition::AllEnemiesDead;
        // NPC is alive but filtered out → players_alive = false, enemies_alive = true
        // This is defeat (party wipe from engine's perspective).
        assert_eq!(determine_outcome(false, true, false, &obj), Some(false));
    }

    /// A living NPC must not satisfy "enemies alive" for AllEnemiesDead objective.
    /// NPCs are on Team::Player, so they can never be in enemies_alive anyway.
    /// This test documents that: enemies_alive=false + players_alive=true → victory.
    #[test]
    fn outcome_npc_does_not_satisfy_enemies_alive_for_all_enemies_dead() {
        let obj = VictoryCondition::AllEnemiesDead;
        // If only NPC and hero alive (npc filtered out of enemies count),
        // enemies_alive = false → victory.
        assert_eq!(determine_outcome(true, false, false, &obj), Some(true));
    }
}
