//! Serde adapters for types without native `Serialize`/`Deserialize`:
//! `bevy::Entity`, `hexx::Hex`, the bit-packed `AiTags`.
//!
//! Apply with `#[serde(with = "crate::combat::ai::log::serde_helpers::hex")]` (and
//! variants for `Vec<Hex>`, `Option<Entity>`, `Vec<Entity>`, `AiTags`).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod hex {
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

pub mod entity {
    use super::*;
    use bevy::prelude::Entity;

    pub fn serialize<S: Serializer>(e: &Entity, s: S) -> Result<S::Ok, S::Error> {
        e.to_bits().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Entity, D::Error> {
        let bits = u64::deserialize(d)?;
        Entity::try_from_bits(bits).ok_or_else(|| serde::de::Error::custom("invalid entity bits"))
    }
}

pub mod entity_opt {
    use super::*;
    use bevy::prelude::Entity;

    pub fn serialize<S: Serializer>(e: &Option<Entity>, s: S) -> Result<S::Ok, S::Error> {
        e.map(|e| e.to_bits()).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Entity>, D::Error> {
        let bits = Option::<u64>::deserialize(d)?;
        bits.map(|b| {
            Entity::try_from_bits(b).ok_or_else(|| serde::de::Error::custom("invalid entity bits"))
        })
        .transpose()
    }
}

pub mod entity_vec {
    use super::*;
    use bevy::prelude::Entity;

    pub fn serialize<S: Serializer>(v: &[Entity], s: S) -> Result<S::Ok, S::Error> {
        let bits: Vec<u64> = v.iter().map(|e| e.to_bits()).collect();
        bits.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Entity>, D::Error> {
        let bits = Vec::<u64>::deserialize(d)?;
        bits.into_iter()
            .map(|b| {
                Entity::try_from_bits(b)
                    .ok_or_else(|| serde::de::Error::custom("invalid entity bits"))
            })
            .collect()
    }
}

pub mod ai_tags {
    use super::*;
    use crate::combat::ai::world::tags::AiTags;

    pub fn serialize<S: Serializer>(t: &AiTags, s: S) -> Result<S::Ok, S::Error> {
        t.bits().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<AiTags, D::Error> {
        let bits = u16::deserialize(d)?;
        Ok(AiTags::from_bits_truncate(bits))
    }
}
