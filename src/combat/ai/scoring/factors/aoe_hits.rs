//! AoE hit enumeration for the AI scoring layer.
//!
//! Parallel to `effects_state::compute_affected_targets` (which lives in the
//! shared core and returns `Vec<Entity>` for sim / real resolution). This
//! helper works on `&UnitSnapshot` references so scoring can read threat /
//! role / HP without re-looking-up, and splits the hits by team relative to
//! the caster.
//!
//! `self_hit` is reported separately from `allies` so callers don't have to
//! remember that `BattleSnapshot::allies_of` includes the actor itself — a
//! trap that previously let `compute_aoe_damage` subtract the caster's
//! friendly-fire damage twice (once via `allies_of`, once via the explicit
//! self-branch).

use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::game::hex::Hex;
use std::collections::HashSet;

/// Units touched by an AoE blast, split by team relative to the caster.
/// `allies` excludes the actor; use `self_hit` for that.
pub struct AoeHits<'a> {
    pub enemies: Vec<&'a UnitSnapshot>,
    pub allies: Vec<&'a UnitSnapshot>,
    pub self_hit: bool,
}

impl AoeHits<'_> {
    /// Count allies including the actor — preserves the "actor-is-an-ally"
    /// semantics used by `generator::is_valid_cast`'s friendly-fire gate.
    pub fn ally_count_with_self(&self) -> usize {
        self.allies.len() + self.self_hit as usize
    }
}

/// Single pass over the snapshot: bucket each unit in `area` as enemy / ally /
/// self relative to `active`. `area` is the pre-computed `aoe_area` result,
/// reused across multiple classifications of the same blast.
pub fn aoe_hits<'a>(
    area: &HashSet<Hex>,
    active: &UnitSnapshot,
    snap: &'a BattleSnapshot,
) -> AoeHits<'a> {
    let mut enemies = Vec::new();
    let mut allies = Vec::new();
    let mut self_hit = false;
    for u in &snap.units {
        if !area.contains(&u.pos) {
            continue;
        }
        if u.entity == active.entity {
            self_hit = true;
            continue;
        }
        if u.team == active.team {
            allies.push(u);
        } else {
            enemies.push(u);
        }
    }
    AoeHits { enemies, allies, self_hit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::unit;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn enemies_allies_and_self_are_separated() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let ally = unit(2, Team::Enemy, hex_from_offset(1, 0));
        let enemy = unit(3, Team::Player, hex_from_offset(0, 1));
        let offscreen_enemy = unit(4, Team::Player, hex_from_offset(5, 5));

        let snap = snapshot_from(
            vec![actor.clone(), ally.clone(), enemy.clone(), offscreen_enemy],
            1,
        );
        let area: HashSet<Hex> = [actor.pos, ally.pos, enemy.pos].into_iter().collect();

        let hits = aoe_hits(&area, &actor, &snap);
        assert_eq!(hits.enemies.len(), 1);
        assert_eq!(hits.enemies[0].entity, enemy.entity);
        assert_eq!(hits.allies.len(), 1);
        assert_eq!(hits.allies[0].entity, ally.entity);
        assert!(hits.self_hit);
    }

    /// Regression guard: the actor must never appear in `allies`. Before this
    /// helper existed, `compute_aoe_damage` iterated `allies_of(team)` (which
    /// includes the caster) AND then subtracted self-damage explicitly —
    /// double-penalising the caster for standing in their own AoE.
    #[test]
    fn actor_never_leaks_into_allies() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let snap = snapshot_from(vec![actor.clone()], 1);
        let area: HashSet<Hex> = [actor.pos].into_iter().collect();

        let hits = aoe_hits(&area, &actor, &snap);
        assert!(hits.allies.is_empty(), "actor must not be counted as an ally");
        assert!(hits.self_hit);
        assert_eq!(hits.ally_count_with_self(), 1);
    }

    #[test]
    fn units_outside_area_are_ignored() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let far_enemy = unit(2, Team::Player, hex_from_offset(10, 10));
        let snap = snapshot_from(vec![actor.clone(), far_enemy], 1);
        let area: HashSet<Hex> = [actor.pos].into_iter().collect();

        let hits = aoe_hits(&area, &actor, &snap);
        assert!(hits.enemies.is_empty());
        assert!(hits.allies.is_empty());
        assert!(hits.self_hit);
    }
}
