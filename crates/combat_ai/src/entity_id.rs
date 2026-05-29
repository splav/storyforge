//! `EntityId` — wire-compatible newtype над `bevy::Entity::to_bits()`.
//!
//! Используется в чистой логике AI как actor/target ID. На границе с
//! Bevy ECS (будущий `src/combat/ai_bridge/`) конвертится в `bevy::Entity`
//! через `From`/`TryFrom`, определённые в bridge — чтобы `combat_ai` не
//! зависел от `bevy_ecs::entity::Entity` (он внутри bevy_ecs, не в pure
//! части).
//!
//! Сериализация transparent — поверх `u64`. Это критично для wire-compat
//! с JSONL-логами AI (текущие логи хранят actor/target как `u64 bits`).

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct EntityId(pub u64);

impl EntityId {
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }
    pub const fn to_bits(self) -> u64 {
        self.0
    }
}

impl From<u64> for EntityId {
    fn from(bits: u64) -> Self {
        Self(bits)
    }
}

impl From<EntityId> for u64 {
    fn from(id: EntityId) -> u64 {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire-compat: serialized form must be just the bare u64.
    /// Если эта инвариантa нарушена, JSONL-логи AI становятся несовместимы.
    #[test]
    fn serde_is_transparent_to_u64() {
        let id = EntityId(0xDEAD_BEEF_CAFE_F00D);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "16045690984503111693");
        let back: EntityId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }

}
