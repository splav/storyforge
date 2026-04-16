use bevy::prelude::*;
use crate::game::hex::Hex;
use std::collections::HashMap;

/// Shared mutable state within a single enemy phase (one round).
/// Tracks what previous AI units have "claimed" so subsequent units
/// can avoid overkill, duplicate CC, and tile collisions.
#[derive(Resource, Default)]
pub struct Reservations {
    damage: HashMap<Entity, f32>,
    cc: HashMap<Entity, u32>,
    tiles: HashMap<Hex, Entity>,
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
        *self.cc.entry(target).or_default() += 1;
    }

    pub fn reserve_tile(&mut self, tile: Hex, actor: Entity) {
        self.tiles.insert(tile, actor);
    }

    pub fn reserved_damage(&self, target: Entity) -> f32 {
        self.damage.get(&target).copied().unwrap_or(0.0)
    }

    pub fn reserved_cc(&self, target: Entity) -> u32 {
        self.cc.get(&target).copied().unwrap_or(0)
    }

    pub fn is_tile_reserved(&self, tile: Hex) -> bool {
        self.tiles.contains_key(&tile)
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
        r.reserve_tile(tile, e);

        r.clear();

        assert_eq!(r.reserved_damage(e), 0.0);
        assert_eq!(r.reserved_cc(e), 0);
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
    fn cc_stacking() {
        let mut r = Reservations::default();
        let e = entity(1);

        r.reserve_cc(e);
        r.reserve_cc(e);

        assert_eq!(r.reserved_cc(e), 2);
    }

    #[test]
    fn tile_reservation() {
        let mut r = Reservations::default();
        let tile = hex_from_offset(4, 2);
        let actor = entity(5);

        assert!(!r.is_tile_reserved(tile));
        r.reserve_tile(tile, actor);
        assert!(r.is_tile_reserved(tile));
    }

    #[test]
    fn queries_return_zero_for_unknown() {
        let r = Reservations::default();
        let e = entity(99);
        let tile = hex_from_offset(0, 0);

        assert_eq!(r.reserved_damage(e), 0.0);
        assert_eq!(r.reserved_cc(e), 0);
        assert!(!r.is_tile_reserved(tile));
    }
}
