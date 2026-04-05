use crate::core::{DiceExpr, WeaponId};

pub const WEAPON_SHORT_SWORD: WeaponId = WeaponId(1);
pub const WEAPON_LONG_SWORD:  WeaponId = WeaponId(2);

#[derive(Debug, Clone)]
pub struct WeaponDef {
    pub id:   WeaponId,
    pub name: &'static str,
    pub dice: DiceExpr,   // damage dice (flat bonus comes from CombatStats.damage)
}

pub fn default_weapons() -> Vec<WeaponDef> {
    vec![
        WeaponDef {
            id:   WEAPON_SHORT_SWORD,
            name: "Короткий меч",
            dice: DiceExpr::new(1, 6, 0),
        },
        WeaponDef {
            id:   WEAPON_LONG_SWORD,
            name: "Длинный меч",
            dice: DiceExpr::new(1, 8, 0),
        },
    ]
}
