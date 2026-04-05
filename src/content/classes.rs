use crate::core::{AbilityId, WeaponId};
use crate::content::abilities::{ABILITY_SWORD_ATTACK, ABILITY_SHIELD_BLOCK};
use crate::content::weapons::WEAPON_LONG_SWORD;
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
        stats: CombatStats { max_hp: 20, armor: 3, damage: 4, initiative: 6 },
        abilities: vec![ABILITY_SWORD_ATTACK, ABILITY_SHIELD_BLOCK],
        weapon: WEAPON_LONG_SWORD,
    }
}
