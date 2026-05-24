//! Serde adapters for `EntityId`-bearing types.
//!
//! Зеркалит интерфейс `src/combat/ai/log/serde_helpers.rs` (`entity`,
//! `entity_opt`, `entity_vec`), но работает с `EntityId` вместо `bevy::Entity`.
//! Используется будущими модулями AI после миграции в Phase 3.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::EntityId;

pub mod entity {
    use super::*;

    pub fn serialize<S: Serializer>(id: &EntityId, s: S) -> Result<S::Ok, S::Error> {
        id.to_bits().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<EntityId, D::Error> {
        let bits = u64::deserialize(d)?;
        Ok(EntityId::from_bits(bits))
    }
}

pub mod entity_opt {
    use super::*;

    pub fn serialize<S: Serializer>(id: &Option<EntityId>, s: S) -> Result<S::Ok, S::Error> {
        id.map(|i| i.to_bits()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<EntityId>, D::Error> {
        let bits = Option::<u64>::deserialize(d)?;
        Ok(bits.map(EntityId::from_bits))
    }
}

pub mod entity_vec {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[EntityId], s: S) -> Result<S::Ok, S::Error> {
        let bits: Vec<u64> = v.iter().map(|i| i.to_bits()).collect();
        bits.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<EntityId>, D::Error> {
        let bits = Vec::<u64>::deserialize(d)?;
        Ok(bits.into_iter().map(EntityId::from_bits).collect())
    }
}

pub mod hex {
    //! Re-export hex/hex_vec adapters матчинг текущий
    //! `src/combat/ai/log/serde_helpers::hex`. Hex переедет вместе с
    //! AI-типами в Phase 2-3, но адаптер нужен здесь сразу — на него
    //! ссылаются типы AI после миграции.
    use super::*;
    use hexx::Hex;

    pub fn serialize<S: Serializer>(h: &Hex, s: S) -> Result<S::Ok, S::Error> {
        [h.x, h.y].serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hex, D::Error> {
        let pair = <[i32; 2]>::deserialize(d)?;
        Ok(Hex::new(pair[0], pair[1]))
    }
}

pub mod hex_vec {
    use super::*;
    use hexx::Hex;

    pub fn serialize<S: Serializer>(v: &[Hex], s: S) -> Result<S::Ok, S::Error> {
        let pairs: Vec<[i32; 2]> = v.iter().map(|h| [h.x, h.y]).collect();
        pairs.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Hex>, D::Error> {
        let pairs = Vec::<[i32; 2]>::deserialize(d)?;
        Ok(pairs.into_iter().map(|p| Hex::new(p[0], p[1])).collect())
    }
}
