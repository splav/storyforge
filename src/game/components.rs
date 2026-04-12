use crate::core::{AbilityId, StatusId, WeaponId};
use bevy::prelude::*;

#[derive(Component, Default)]
pub struct Combatant;

/// Starting hex grid position (col, row) assigned at spawn.
#[derive(Component, Clone, Copy)]
pub struct StartingHexPos(pub i32, pub i32);

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
    pub max_hp: i32,
    pub armor: i32,
    pub strength: i32,  // melee attack/damage bonus
    pub dexterity: i32, // initiative bonus
    pub constitution: i32,
    pub intelligence: i32, // boosts spell damage and healing
    pub wisdom: i32,
    pub charisma: i32,
}

#[derive(Component)]
pub struct Vital {
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32, // reduces incoming damage
}

impl Vital {
    pub fn new(stats: &CombatStats) -> Self {
        Self {
            hp: stats.max_hp,
            max_hp: stats.max_hp,
            armor: stats.armor,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    pub fn apply_damage(&mut self, amount: i32) {
        self.hp = (self.hp - amount).max(0);
    }

    pub fn apply_heal(&mut self, amount: i32) {
        self.hp = (self.hp + amount).min(self.max_hp);
    }
}

/// How many hex cells the unit can move per turn.
#[derive(Component, Clone, Copy, Debug)]
pub struct Speed(pub i32);

/// Temporary extra movement granted by abilities (e.g. Rush).
/// Removed after the bonus move is spent.
#[derive(Component)]
pub struct BonusMovement(pub i32);

#[derive(Component)]
pub struct Initiative(pub i32);

#[derive(Component)]
pub struct ActionPoints {
    pub action: bool,
    pub movement: bool,
}

impl Default for ActionPoints {
    fn default() -> Self {
        Self {
            action: true,
            movement: true,
        }
    }
}

#[derive(Component, Default)]
pub struct Abilities(pub Vec<AbilityId>);

/// Ярость — накапливается при ударах и получении урона.
/// Присутствует только у персонажей с этой механикой (воин).
#[derive(Component, Debug, Clone)]
pub struct Rage {
    pub current: i32,
    pub max: i32,
}

/// Мана — расходуется на заклинания, восстанавливается на 1 в конце каждого хода.
/// Присутствует только у персонажей с этой механикой (маг).
#[derive(Component, Debug, Clone)]
pub struct Mana {
    pub current: i32,
    pub max: i32,
}

impl Mana {
    pub fn new(max: i32) -> Self {
        Self { current: max, max }
    }

    /// Восстановить amount маны (не выше max). Возвращает новое значение.
    pub fn restore(&mut self, amount: i32) -> i32 {
        self.current = (self.current + amount).min(self.max);
        self.current
    }

    /// Потратить ману. Возвращает false если недостаточно.
    pub fn spend(&mut self, amount: i32) -> bool {
        if self.current < amount {
            return false;
        }
        self.current -= amount;
        true
    }
}

impl Rage {
    pub fn new(max: i32) -> Self {
        Self { current: 0, max }
    }

    /// Прибавить 1 ярость (не выше max). Возвращает новое значение.
    pub fn gain(&mut self) -> i32 {
        self.current = (self.current + 1).min(self.max);
        self.current
    }

    /// Потратить ярость. Возвращает false если недостаточно.
    pub fn spend(&mut self, amount: i32) -> bool {
        if self.current < amount {
            return false;
        }
        self.current -= amount;
        true
    }
}

/// The weapon currently equipped by this combatant.
#[derive(Component, Clone)]
pub struct EquippedWeapon(pub WeaponId);

#[derive(Component, Default)]
pub struct StatusEffects(pub Vec<ActiveStatus>);

#[derive(Debug, Clone)]
pub struct ActiveStatus {
    pub id: StatusId,
    pub rounds_remaining: u32,
    /// Entity whose EndTurn ticks this counter down.
    pub applier: Entity,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vital(hp: i32, max_hp: i32) -> Vital {
        Vital {
            hp,
            max_hp,
            armor: 0,
        }
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
