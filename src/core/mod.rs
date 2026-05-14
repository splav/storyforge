pub use combat_engine::{AbilityId, ArmorId, StatusId, WeaponId};
pub use combat_engine::{DiceExpr, DiceRng};
pub use combat_engine::ResourceKind;

/// Модификатор характеристики: floor(stat / 2).
/// Диапазон характеристик −5..10 → модификаторы −3..+5.
pub fn modifier(stat: i32) -> i32 {
    stat >> 1 // арифметический сдвиг = floor для степеней двойки
}
