use bevy::prelude::*;
use crate::core::{AbilityId, StatusId, WeaponId};

#[derive(Component, Default)]
pub struct Combatant;

/// Inserted when hp reaches 0. Skips the unit's turn and prevents acting.
#[derive(Component, Default)]
pub struct Dead;

#[derive(Component, Default)]
pub struct PartyMember;

#[derive(Component, Default)]
pub struct Enemy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Team {
    Player,
    Enemy,
}

#[derive(Component)]
pub struct Faction(pub Team);

/// The core combat stats.
#[derive(Component, Clone, Debug)]
pub struct CombatStats {
    pub max_hp:       i32,
    pub armor:        i32,
    pub damage:       i32,
    pub initiative:   i32,   // base value; rolled initiative = initiative + d20
    pub intelligence: i32,   // boosts spell damage and healing
}

#[derive(Component)]
pub struct Vital {
    pub hp:      i32,
    pub max_hp:  i32,
    pub armor:   i32,   // reduces incoming damage
}

impl Vital {
    pub fn new(stats: &CombatStats) -> Self {
        Self { hp: stats.max_hp, max_hp: stats.max_hp, armor: stats.armor }
    }

    pub fn is_alive(&self) -> bool { self.hp > 0 }

    pub fn apply_damage(&mut self, amount: i32) {
        self.hp = (self.hp - amount).max(0);
    }

    pub fn apply_heal(&mut self, amount: i32) {
        self.hp = (self.hp + amount).min(self.max_hp);
    }
}

#[derive(Component)]
pub struct Initiative(pub i32);

#[derive(Component)]
pub struct ActionPoints {
    pub action: bool,
}

impl Default for ActionPoints {
    fn default() -> Self { Self { action: true } }
}

#[derive(Component, Default)]
pub struct Abilities(pub Vec<AbilityId>);

/// The weapon currently equipped by this combatant.
#[derive(Component, Clone)]
pub struct EquippedWeapon(pub WeaponId);

#[derive(Component, Default)]
pub struct StatusEffects(pub Vec<ActiveStatus>);

#[derive(Debug, Clone)]
pub struct ActiveStatus {
    pub id: StatusId,
    pub rounds_remaining: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vital(hp: i32, max_hp: i32) -> Vital {
        Vital { hp, max_hp, armor: 0 }
    }

    #[test]
    fn damage_does_not_go_below_zero() {
        let mut v = vital(5, 10);
        v.apply_damage(100);
        assert_eq!(v.hp, 0);
    }

    #[test]
    fn damage_reduces_hp() {
        let mut v = vital(10, 10);
        v.apply_damage(3);
        assert_eq!(v.hp, 7);
    }

    #[test]
    fn heal_does_not_exceed_max_hp() {
        let mut v = vital(1, 10);
        v.apply_heal(100);
        assert_eq!(v.hp, 10);
    }

    #[test]
    fn is_alive_false_at_zero_hp() {
        assert!(!vital(0, 10).is_alive());
    }

    #[test]
    fn is_alive_true_above_zero() {
        assert!(vital(1, 10).is_alive());
    }
}
