use crate::game::components::CombatStats;

/// Additive stat bundle an item can carry (the "что может быть на предмете"
/// shared core). `combat` reuses the character's own `CombatStats` so item
/// bonuses fold straight onto base stats. `armor`/`mana` are flat pool/defense
/// bonuses. `magic_resist` is flat mitigation of MAGIC damage (mirrors `armor`
/// for physical). Weapon- and armor-specific fields (spell_power/dice/hand,
/// slot/weight) stay on their respective Def structs.
#[derive(Debug, Clone, Default)]
pub struct ItemStats {
    pub combat: CombatStats,
    pub armor: i32,
    pub mana: i32,
    pub magic_resist: i32,
}
