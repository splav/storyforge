use crate::game::resources::{HexCorpses, HexPositions};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

/// Spatial-map façade unifying the two occupancy layers:
///
/// - **living units** (one-per-hex, [`HexPositions`])
/// - **corpses** (multi-occupant, [`HexCorpses`])
///
/// Readers should access the map through this façade so that the two-layer
/// model stays encapsulated. Pick the method by **intent**:
///
/// - [`living_at`]    — blocking, targeting (regular abilities), AoO sources.
/// - [`corpses_at`]   — loot, raise-dead, cleave-over-corpse.
/// - [`any_at`]       — UI fill / tooltip / labels (living first, then first corpse).
/// - [`position_of`]  — "where is this entity now?" — checks both layers.
/// - [`iter_living`]  — iterate all living unit positions.
///
/// **Writers** (projector, despawn, spawn) keep direct `ResMut<HexPositions>` /
/// `ResMut<HexCorpses>` — explicit layer choice is correct there.
#[derive(SystemParam)]
pub struct HexMap<'w> {
    positions: Res<'w, HexPositions>,
    corpses: Res<'w, HexCorpses>,
}

impl<'w> HexMap<'w> {
    /// Returns the living unit at `pos`, or `None` if the hex is empty (or has only corpses).
    pub fn living_at(&self, pos: hexx::Hex) -> Option<Entity> {
        self.positions.entity_at(pos)
    }

    /// Returns all corpses at `pos` (may be empty).
    pub fn corpses_at(&self, pos: hexx::Hex) -> &[Entity] {
        self.corpses.at(&pos)
    }

    /// Returns the living unit at `pos`, or — if none — the first corpse.
    /// Used for UI fill / tooltip / labels where either occupant is displayable.
    pub fn any_at(&self, pos: hexx::Hex) -> Option<Entity> {
        self.positions
            .entity_at(pos)
            .or_else(|| self.corpses.at(&pos).first().copied())
    }

    /// Returns the hex occupied by `entity` in either layer, or `None` if not found.
    pub fn position_of(&self, entity: Entity) -> Option<hexx::Hex> {
        self.positions
            .get(&entity)
            .or_else(|| self.corpses.get(&entity))
    }

    /// Iterates all living unit `(entity, hex)` pairs.
    pub fn iter_living(&self) -> impl Iterator<Item = (&Entity, &hexx::Hex)> {
        self.positions.iter()
    }
}
