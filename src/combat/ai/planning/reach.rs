//! Shared BFS reach helper for the AI planner.
//!
//! Both the beam-search generator (which walks a `SimState`) and the fallback
//! move handler (which starts from the original snapshot) used to ship their
//! own copies of the same pathfinding setup: pull enemy positions for
//! `is_passable`, pull all non-self occupied tiles + corpses for
//! `can_stop_on`, and hand them to `reachable_with_paths`. One place, not two.

use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::game::hex::{can_stop_on, is_passable, Hex};
use crate::game::pathfinding::{reachable_with_paths, ReachableMap};
use std::collections::HashSet;

/// BFS from `actor.pos` with the planner's passability / stop rules:
///
/// - `is_passable` rejects enemy-occupied tiles (walk-through blocked).
/// - `can_stop_on` rejects any non-actor occupied tile, plus `blocked_tiles`
///   (which carries real-world corpses the snapshot already pruned).
///
/// The actor itself is never added to the occupied set — standing on one's
/// own tile is legal, and a zero-MP reach should still include it.
pub fn reach_from(
    snap: &BattleSnapshot,
    actor: &UnitSnapshot,
    blocked_tiles: &HashSet<Hex>,
) -> ReachableMap {
    let enemy_positions: HashSet<Hex> = snap
        .enemies_of(actor.team)
        .map(|u| u.pos)
        .collect();
    let mut all_occupied: HashSet<Hex> = snap
        .units
        .iter()
        .filter(|u| u.entity != actor.entity)
        .map(|u| u.pos)
        .collect();
    all_occupied.extend(blocked_tiles.iter().copied());

    reachable_with_paths(
        actor.pos,
        actor.movement_points,
        move |h| is_passable(h, &enemy_positions),
        move |h| can_stop_on(h, &all_occupied, None),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::AiTags;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(id).expect("valid"),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 1,
            max_ap: 1,
            movement_points: 3,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot { units, round: 1 }
    }

    #[test]
    fn neighbour_tile_is_reachable_with_mp() {
        // Actor has MP 3 → every empty neighbour should be in destinations.
        // (destinations excludes the start tile by design.)
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let s = snap(vec![actor.clone()]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        let neighbour = hex_from_offset(4, 3);
        assert!(
            reach.destinations.contains(&neighbour),
            "empty neighbour should be reachable (got {:?})",
            reach.destinations,
        );
    }

    #[test]
    fn enemy_tile_is_not_stoppable() {
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let enemy_pos = hex_from_offset(4, 3);
        let enemy = unit(2, Team::Player, enemy_pos);
        let s = snap(vec![actor.clone(), enemy]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        assert!(
            !reach.destinations.contains(&enemy_pos),
            "occupied enemy tile must not be a stop destination",
        );
    }

    #[test]
    fn ally_tile_is_not_stoppable_but_passable() {
        // Teammate blocks stopping on their tile but lets the actor pass
        // through when pathing to something beyond.
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let ally_pos = hex_from_offset(4, 3);
        let ally = unit(2, Team::Enemy, ally_pos);
        let s = snap(vec![actor.clone(), ally]);
        let reach = reach_from(&s, &actor, &HashSet::new());
        assert!(
            !reach.destinations.contains(&ally_pos),
            "can't stop on ally",
        );
        // Beyond the ally should still be reachable via pass-through.
        let beyond = hex_from_offset(5, 3);
        assert!(
            reach.destinations.contains(&beyond),
            "beyond-ally tile should be reachable",
        );
    }

    #[test]
    fn blocked_tile_is_rejected_as_stop() {
        // A corpse (present in blocked_tiles but not in snapshot.units)
        // must still be treated as occupied for stopping.
        let actor = unit(1, Team::Enemy, hex_from_offset(3, 3));
        let corpse = hex_from_offset(4, 3);
        let s = snap(vec![actor.clone()]);
        let mut blocked = HashSet::new();
        blocked.insert(corpse);
        let reach = reach_from(&s, &actor, &blocked);
        assert!(
            !reach.destinations.contains(&corpse),
            "corpse tile must not be a stop destination",
        );
    }
}
