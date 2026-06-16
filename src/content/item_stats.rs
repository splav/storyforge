use crate::game::components::CombatStats;

/// Additive stat bundle an item can carry. `combat` reuses the character's own
/// `CombatStats` so bonuses fold onto base stats; `magic_resist` mitigates MAGIC
/// damage as `armor` does physical. Weapon/armor-specific fields stay on their
/// respective Def structs.
#[derive(Debug, Clone, Default)]
pub struct ItemStats {
    pub combat: CombatStats,
    pub armor: i32,
    pub mana: i32,
    pub magic_resist: i32,
}
