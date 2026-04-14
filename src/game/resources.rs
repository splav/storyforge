use crate::content::abilities::{load_abilities, AbilityDef};
use crate::content::classes::{load_classes, ClassDef};
use crate::content::encounters::{load_encounters, EncounterDef};
use crate::content::scenarios::{load_scenarios, ScenarioDef};
use crate::content::statuses::{load_statuses, StatusDef};
use crate::content::weapons::{load_weapons, WeaponDef};
use crate::core::{AbilityId, StatusId, WeaponId};
use bevy::prelude::*;
use std::collections::HashMap;

// ── Initiative preset (carry initiative across a combat restart) ─────────────

/// Populated before a combat restart. Maps combatant name → saved initiative value.
/// `build_turn_order` reads this on round 1 instead of rolling, then clears it.
#[derive(Resource, Default)]
pub struct PresetInitiative(pub HashMap<String, i32>);

// ── Combat runtime ───────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct CombatContext {
    pub round: u32,
    pub encounter: Option<Entity>,
}

#[derive(Resource, Default)]
pub struct TurnQueue {
    pub order: Vec<Entity>,
    pub index: usize,
}

impl TurnQueue {
    pub fn current(&self) -> Option<Entity> {
        self.order.get(self.index).copied()
    }

    pub fn advance(&mut self) {
        self.index = (self.index + 1) % self.order.len().max(1);
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use bevy::prelude::Entity;

    fn dummy(id: u32) -> Entity {
        // id 0..u32::MAX-1 are valid (u32::MAX is NonMaxU32's sentinel)
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    #[test]
    fn current_none_on_empty_queue() {
        assert!(TurnQueue::default().current().is_none());
    }

    #[test]
    fn advance_wraps_around() {
        let mut q = TurnQueue {
            order: vec![dummy(0), dummy(1)],
            index: 1,
        };
        q.advance();
        assert_eq!(q.index, 0);
    }

    #[test]
    fn advance_on_empty_stays_zero() {
        let mut q = TurnQueue::default();
        q.advance(); // max(1) guard — should not panic
        assert_eq!(q.index, 0);
    }

    #[test]
    fn current_returns_active_entity() {
        let e = dummy(42);
        let q = TurnQueue {
            order: vec![e],
            index: 0,
        };
        assert_eq!(q.current(), Some(e));
    }
}

// ── Game rules database ──────────────────────────────────────────────────────

#[derive(Resource)]
pub struct GameDb {
    pub abilities: HashMap<AbilityId, AbilityDef>,
    pub statuses: HashMap<StatusId, StatusDef>,
    pub weapons: HashMap<WeaponId, WeaponDef>,
    pub encounters: HashMap<String, EncounterDef>,
    pub classes: HashMap<String, ClassDef>,
    pub scenarios: HashMap<String, ScenarioDef>,
}

impl Default for GameDb {
    fn default() -> Self {
        let db = Self {
            abilities: load_abilities()
                .into_iter()
                .map(|a| (a.id.clone(), a))
                .collect(),
            statuses: load_statuses()
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect(),
            weapons: load_weapons()
                .into_iter()
                .map(|w| (w.id.clone(), w))
                .collect(),
            encounters: load_encounters()
                .into_iter()
                .map(|e| (e.id.clone(), e))
                .collect(),
            classes: load_classes()
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect(),
            scenarios: load_scenarios()
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect(),
        };
        db.validate();
        db
    }
}

impl GameDb {
    /// Validate cross-references between TOML data files.
    /// Panics with a clear message if any reference is broken.
    fn validate(&self) {
        // Abilities → statuses
        for (id, def) in &self.abilities {
            for sa in &def.statuses {
                assert!(
                    self.statuses.contains_key(&sa.status),
                    "ability '{}' references unknown status '{}'",
                    id,
                    sa.status
                );
            }
        }

        // Classes → weapons, abilities
        for (id, cls) in &self.classes {
            assert!(
                self.weapons.contains_key(&cls.weapon),
                "class '{}' references unknown weapon '{}'",
                id,
                cls.weapon
            );
            for aid in &cls.abilities {
                assert!(
                    self.abilities.contains_key(aid),
                    "class '{}' references unknown ability '{}'",
                    id,
                    aid
                );
            }
        }

        // Encounters → weapons, abilities
        for (id, enc) in &self.encounters {
            for enemy in &enc.enemies {
                assert!(
                    self.weapons.contains_key(&enemy.weapon_id),
                    "encounter '{}' enemy '{}' references unknown weapon '{}'",
                    id,
                    enemy.name,
                    enemy.weapon_id
                );
                for aid in &enemy.ability_ids {
                    assert!(
                        self.abilities.contains_key(aid),
                        "encounter '{}' enemy '{}' references unknown ability '{}'",
                        id,
                        enemy.name,
                        aid
                    );
                }
            }
        }

        // Scenarios → encounters, classes
        for (id, scen) in &self.scenarios {
            for member in &scen.party {
                assert!(
                    self.classes.contains_key(&member.class_id),
                    "scenario '{}' party member '{}' references unknown class '{}'",
                    id,
                    member.name,
                    member.class_id
                );
            }
            for scene in &scen.scenes {
                if let crate::content::scenarios::SceneDef::Combat { encounter_id } = scene {
                    assert!(
                        self.encounters.contains_key(encounter_id.as_str()),
                        "scenario '{}' references unknown encounter '{}'",
                        id,
                        encounter_id
                    );
                }
            }
        }
    }
}

// ── Scenario state ──────────────────────────────────────────────────────────

#[derive(Resource)]
pub struct ScenarioState {
    pub scenario_id: String,
    pub scene_index: usize,
}

// ── Hex positions ────────────────────────────────────────────────────────────

/// Bidirectional map: entity ↔ hex position (col, row).
#[derive(Resource, Default)]
pub struct HexPositions {
    by_entity: HashMap<Entity, (i32, i32)>,
    by_pos: HashMap<(i32, i32), Entity>,
    pub generation: u64,
}

impl HexPositions {
    pub fn insert(&mut self, entity: Entity, pos: (i32, i32)) {
        if let Some(&old_pos) = self.by_entity.get(&entity) {
            self.by_pos.remove(&old_pos);
        }
        self.by_entity.insert(entity, pos);
        self.by_pos.insert(pos, entity);
        self.generation += 1;
    }

    pub fn remove(&mut self, entity: &Entity) {
        if let Some(pos) = self.by_entity.remove(entity) {
            self.by_pos.remove(&pos);
        }
        self.generation += 1;
    }

    pub fn clear(&mut self) {
        self.by_entity.clear();
        self.by_pos.clear();
        self.generation += 1;
    }

    pub fn get(&self, entity: &Entity) -> Option<(i32, i32)> {
        self.by_entity.get(entity).copied()
    }

    pub fn entity_at(&self, q: i32, r: i32) -> Option<Entity> {
        self.by_pos.get(&(q, r)).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Entity, &(i32, i32))> {
        self.by_entity.iter()
    }
}

// ── UI selection ─────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct SelectionState {
    pub selected_actor: Option<Entity>,
    pub selected_ability: Option<AbilityId>,
    pub selected_target: Option<Entity>,
    pub move_mode: bool,
}

impl SelectionState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

// ── UI dirty flags ──────────────────────────────────────────────────────────

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct UiDirtyFlags: u16 {
        const OVERLAY       = 0b0000_0001;
        const HEX_FILL      = 0b0000_0010;
        const LABELS        = 0b0000_0100;
        const ABILITY_PANEL = 0b0000_1000;
        const TURN_ORDER    = 0b0001_0000;
        const PHASE_HINT    = 0b0010_0000;
        const MOVE_BTN      = 0b0100_0000;
        const TOOLTIP       = 0b1000_0000;
        const TOKENS        = 0b1_0000_0000;
    }
}

#[derive(Resource, Default)]
pub struct UiDirty(pub UiDirtyFlags);
