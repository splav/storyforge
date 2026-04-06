use bevy::prelude::*;
use std::collections::HashMap;
use crate::core::{AbilityId, StatusId, WeaponId};
use crate::content::abilities::{AbilityDef, load_abilities};
use crate::content::classes::{ClassDef, load_classes};
use crate::content::encounters::{EncounterDef, load_encounters};
use crate::content::statuses::{StatusDef, load_statuses};
use crate::content::weapons::{WeaponDef, load_weapons};

// ── Combat runtime ───────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct CombatContext {
    pub round: u32,
    pub active: Option<Entity>,
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
        let mut q = TurnQueue { order: vec![dummy(0), dummy(1)], index: 1 };
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
        let q = TurnQueue { order: vec![e], index: 0 };
        assert_eq!(q.current(), Some(e));
    }
}

// ── Combat log ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CombatEvent {
    CombatStarted,
    RoundStarted { round: u32 },
    TurnStarted { actor: Entity },
    AbilityUsed { actor: Entity, ability_name: String, target: Entity },
    /// Full damage summary: formula + armor reduction + final HP lost.
    DamageResult {
        target:        Entity,
        formula:       String,  // e.g. "1d8=6 + 4(атк) = 10"
        armor_reduced: i32,     // total absorption (armor + statuses)
        final_damage:  i32,     // HP actually lost
    },
    /// Full heal summary: formula + HP actually restored.
    HealResult {
        target:  Entity,
        formula: String,  // e.g. "1d4=2 + 1(сила) + 2(инт) = 5"
        amount:  i32,     // HP actually restored (capped at max_hp)
    },
    Missed { actor: Entity, target: Entity },
    StatusApplied { target: Entity, status: StatusId },
    TurnEnded { actor: Entity },
    CombatEnded { victory: bool },
    UnitDied { entity: Entity },
}

#[derive(Resource, Default)]
pub struct CombatLog(pub Vec<CombatEvent>);

impl CombatLog {
    pub fn push(&mut self, event: CombatEvent) {
        self.0.push(event);
    }
}

// ── Game rules database ──────────────────────────────────────────────────────

#[derive(Resource)]
pub struct GameDb {
    pub abilities:  HashMap<AbilityId, AbilityDef>,
    pub statuses:   HashMap<StatusId, StatusDef>,
    pub weapons:    HashMap<WeaponId, WeaponDef>,
    pub encounters: HashMap<String, EncounterDef>,
    pub classes:    HashMap<String, ClassDef>,
}

impl Default for GameDb {
    fn default() -> Self {
        Self {
            abilities:  load_abilities().into_iter().map(|a| (a.id.clone(), a)).collect(),
            statuses:   load_statuses().into_iter().map(|s| (s.id.clone(), s)).collect(),
            weapons:    load_weapons().into_iter().map(|w| (w.id.clone(), w)).collect(),
            encounters: load_encounters().into_iter().map(|e| (e.id.clone(), e)).collect(),
            classes:    load_classes().into_iter().map(|c| (c.id.clone(), c)).collect(),
        }
    }
}

// ── UI selection ─────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct SelectionState {
    pub selected_actor: Option<Entity>,
    pub selected_ability: Option<AbilityId>,
    pub selected_target: Option<Entity>,
}

impl SelectionState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
