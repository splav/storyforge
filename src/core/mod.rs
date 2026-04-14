pub mod ids;
pub mod rng;

pub use ids::{AbilityId, ArmorId, StatusId, WeaponId};
pub use rng::{DiceExpr, DiceRng};

/// Модификатор характеристики: floor(stat / 2).
/// Диапазон характеристик −5..10 → модификаторы −3..+5.
pub fn modifier(stat: i32) -> i32 {
    stat >> 1 // арифметический сдвиг = floor для степеней двойки
}
