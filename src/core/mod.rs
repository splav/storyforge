pub mod ids;
pub mod rng;

pub use ids::{AbilityId, ArmorId, StatusId, WeaponId};
pub use rng::{DiceExpr, DiceRng};

/// Вид ресурса, который может тратиться на способности.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResourceKind {
    Hp,
    Mana,
    Rage,
    Energy,
}

/// Модификатор характеристики: floor(stat / 2).
/// Диапазон характеристик −5..10 → модификаторы −3..+5.
pub fn modifier(stat: i32) -> i32 {
    stat >> 1 // арифметический сдвиг = floor для степеней двойки
}
