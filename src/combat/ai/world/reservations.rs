use crate::combat::ai::log::ReservationsSnapshot;
use crate::game::hex::Hex;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

/// Shared mutable state within a single enemy phase (one round).
/// Tracks what previous AI units have "claimed" so subsequent units
/// can avoid overkill, duplicate CC, and tile collisions.
///
/// **Lifetime invariant:** cleared at the start of each round by
/// `advance_round_system`. Entries keyed on entities that die
/// mid-round (or tiles that become moot) are **not** proactively
/// pruned — they outlive their subject until the next round-start
/// clear. This is safe by construction because every reader gates
/// on snapshot+legality first:
///
/// - `apply_reservation_adjustments` receives already-legal
///   `ScoredStep`s; `rank_targets` filters dead targets out via
///   `check_legality` before they reach the reservation read, so
///   orphan damage/CC entries have no observable effect on scoring.
/// - Orphan tile reservations stay valid — the tile is still
///   physically occupied by the corpse (snapshot-level `stop_blockers`
///   picks it up regardless of reservation state).
///
/// If this invariant ever needs tightening (e.g., a new consumer
/// queries reservations without a check_legality gate upstream), add
/// proactive cleanup via a `drop_entity` hook on death rather than
/// special-casing per reader.
#[derive(Resource, Default)]
pub struct Reservations {
    damage: HashMap<Entity, f32>,
    cc: HashSet<Entity>,
    tiles: HashSet<Hex>,
}

impl Reservations {
    pub fn clear(&mut self) {
        self.damage.clear();
        self.cc.clear();
        self.tiles.clear();
    }

    pub fn reserve_damage(&mut self, target: Entity, amount: f32) {
        *self.damage.entry(target).or_default() += amount;
    }

    pub fn reserve_cc(&mut self, target: Entity) {
        self.cc.insert(target);
    }

    pub fn reserve_tile(&mut self, tile: Hex) {
        self.tiles.insert(tile);
    }

    pub fn reserved_damage(&self, target: Entity) -> f32 {
        self.damage.get(&target).copied().unwrap_or(0.0)
    }

    pub fn has_reserved_cc(&self, target: Entity) -> bool {
        self.cc.contains(&target)
    }

    pub fn is_tile_reserved(&self, tile: Hex) -> bool {
        self.tiles.contains(&tile)
    }

    /// Capture current state as a serializable snapshot for JSONL logging.
    pub fn to_snapshot(&self) -> ReservationsSnapshot {
        ReservationsSnapshot {
            damage: self.damage.iter().map(|(e, &v)| (e.to_bits(), v)).collect(),
            cc: self.cc.iter().map(|e| e.to_bits()).collect(),
            tiles: self.tiles.iter().map(|h| [h.x, h.y]).collect(),
        }
    }

    /// Restore reservations from a snapshot (used by replay tooling).
    pub fn from_snapshot(snap: &ReservationsSnapshot) -> Self {
        Self {
            damage: snap
                .damage
                .iter()
                .filter_map(|(&bits, &v)| Entity::try_from_bits(bits).map(|e| (e, v)))
                .collect(),
            cc: snap
                .cc
                .iter()
                .filter_map(|&bits| Entity::try_from_bits(bits))
                .collect(),
            tiles: snap.tiles.iter().map(|&[x, y]| Hex::new(x, y)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::hex::hex_from_offset;

    fn entity(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    #[test]
    fn clear_resets_all() {
        let mut r = Reservations::default();
        let e = entity(1);
        let tile = hex_from_offset(3, 3);

        r.reserve_damage(e, 10.0);
        r.reserve_cc(e);
        r.reserve_tile(tile);

        r.clear();

        assert_eq!(r.reserved_damage(e), 0.0);
        assert!(!r.has_reserved_cc(e));
        assert!(!r.is_tile_reserved(tile));
    }

    #[test]
    fn damage_accumulates() {
        let mut r = Reservations::default();
        let e = entity(1);

        r.reserve_damage(e, 8.0);
        r.reserve_damage(e, 12.0);

        assert_eq!(r.reserved_damage(e), 20.0);
    }

    #[test]
    fn cc_is_set_like() {
        let mut r = Reservations::default();
        let e = entity(1);

        assert!(!r.has_reserved_cc(e));
        r.reserve_cc(e);
        r.reserve_cc(e);
        assert!(r.has_reserved_cc(e));
    }

    #[test]
    fn tile_reservation() {
        let mut r = Reservations::default();
        let tile = hex_from_offset(4, 2);

        assert!(!r.is_tile_reserved(tile));
        r.reserve_tile(tile);
        assert!(r.is_tile_reserved(tile));
    }

    #[test]
    fn queries_return_zero_for_unknown() {
        let r = Reservations::default();
        let e = entity(99);
        let tile = hex_from_offset(0, 0);

        assert_eq!(r.reserved_damage(e), 0.0);
        assert!(!r.has_reserved_cc(e));
        assert!(!r.is_tile_reserved(tile));
    }
}
