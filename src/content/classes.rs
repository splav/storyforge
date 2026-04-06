use crate::core::{AbilityId, WeaponId};
use crate::content::abilities::{ABILITY_FIREBALL, ABILITY_HEAL, ABILITY_SHIELD_BLOCK, ABILITY_SWORD_ATTACK};
use crate::content::weapons::{WEAPON_LONG_SWORD, WEAPON_STAFF};
use crate::game::components::CombatStats;

pub struct ClassDef {
    pub name:      &'static str,
    pub stats:     CombatStats,
    pub abilities: Vec<AbilityId>,
    pub weapon:    WeaponId,
}

pub fn warrior() -> ClassDef {
    ClassDef {
        name: "Warrior",
        stats: CombatStats { max_hp: 20, armor: 3, damage: 4, initiative: 6, intelligence: 0 },
        abilities: vec![ABILITY_SWORD_ATTACK.into(), ABILITY_SHIELD_BLOCK.into()],
        weapon:    WEAPON_LONG_SWORD.into(),
    }
}

pub fn mage() -> ClassDef {
    ClassDef {
        name: "Mage",
        stats: CombatStats { max_hp: 12, armor: 0, damage: 0, initiative: 5, intelligence: 2 },
        abilities: vec![
            ABILITY_SWORD_ATTACK.into(),
            ABILITY_FIREBALL.into(),
            ABILITY_HEAL.into(),
        ],
        weapon: WEAPON_STAFF.into(),
    }
}
