//! `AiCache` — per-unit AI-derived metrics, keyed by `Entity`.
//!
//! Populated once at `build_snapshot` time from the same data as `UnitSnapshot`.
//! Read by scoring/intent stages (Phase C). Distinct from gameplay state
//! (which lives in `CombatState`).
//!
//! Serialization: `AiCache.units` is a `Vec<UnitAiCache>` (JSON-readable).
//! `AiCache.by_entity` is `#[serde(skip)]` — an O(1) index rebuilt lazily
//! (same pattern as `BattleSnapshot.by_entity`).

use bevy::prelude::Entity;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::config::tuning::AiTuningOverride;
use crate::content::races::CritFailEffect;
use crate::combat::ai::world::tags::AiTags;
use combat_engine::AbilityId;

// ── AiTags serde helpers ──────────────────────────────────────────────────────

fn default_ai_tags() -> AiTags {
    AiTags::empty()
}

mod serde_ai_tags {
    use super::*;
    use serde::{Serializer, Deserializer};

    pub fn serialize<S: Serializer>(t: &AiTags, s: S) -> Result<S::Ok, S::Error> {
        t.bits().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<AiTags, D::Error> {
        let bits = u16::deserialize(d)?;
        Ok(AiTags::from_bits_truncate(bits))
    }
}

// ── Entity serde helpers ──────────────────────────────────────────────────────

mod serde_entity {
    use super::*;
    use serde::{Serializer, Deserializer};

    pub fn serialize<S: Serializer>(e: &Entity, s: S) -> Result<S::Ok, S::Error> {
        e.to_bits().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Entity, D::Error> {
        let bits = u64::deserialize(d)?;
        Entity::try_from_bits(bits)
            .ok_or_else(|| serde::de::Error::custom("invalid entity bits"))
    }
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Per-unit AI-derived metrics. Contains exactly the fields that the AI
/// scoring and intent layers need — gameplay state lives in `CombatState`.
///
/// After Phase D, this is the single source of AI-derived data per unit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnitAiCache {
    /// Entity bits stored inline so each row is self-contained in JSON.
    #[serde(with = "serde_entity")]
    pub entity: Entity,
    pub role: AxisProfile,
    pub threat: f32,
    #[serde(with = "serde_ai_tags", default = "default_ai_tags")]
    pub tags: AiTags,
    pub max_attack_range: u32,
    pub aoo_expected_damage: Option<f32>,
    #[serde(default)]
    pub damage_horizon: Vec<f32>,
    #[serde(default)]
    pub crit_fail_effect: CritFailEffect,
    #[serde(default)]
    pub ai_tuning_override: Option<AiTuningOverride>,
    #[serde(default)]
    pub abilities: Vec<AbilityId>,
    /// Caster parameters (str/int mod, spell power, weapon dice). Migrated
    /// from `UnitSnapshot.caster_ctx` in Phase D-step-3.
    /// Schema: absent in pre-Phase-D logs → `CasterContext::default()`.
    #[serde(default)]
    pub caster_ctx: crate::content::abilities::CasterContext,
    /// When set, overrides the evaluation mode for every plan this unit
    /// generates. Sourced from `AiBehaviorOverride` ECS component (set by a
    /// boss phase transition). `None` for normal units.
    /// Schema: additive field, `#[serde(default)]` → `None` on old logs.
    #[serde(default)]
    pub forced_mode: Option<crate::combat::ai::adapt::EvaluationMode>,
}

/// Side-table of AI-derived per-unit metrics. Populated once at
/// `build_snapshot` time; index rebuilt in `build_index`.
///
/// `units` is ordered identically to `BattleSnapshot.units` for the same
/// snapshot, so positional indexing works across both. Use `unit(entity)`
/// for O(1) keyed access.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AiCache {
    pub units: Vec<UnitAiCache>,
    /// O(1) entity → units[index] lookup. `#[serde(skip)]`: rebuilt after
    /// deserialization via `build_index`, or implicitly via `AiCache::from_units`.
    #[serde(skip)]
    by_entity: HashMap<Entity, usize>,
}

impl AiCache {
    /// Build an `AiCache` from a vec of unit records, constructing the index
    /// eagerly.
    pub fn from_units(units: Vec<UnitAiCache>) -> Self {
        let mut cache = Self { units, by_entity: HashMap::new() };
        cache.build_index();
        cache
    }

    /// (Re)build the entity → index map from the current `units` vec. Call
    /// after deserialization when O(1) access is needed.
    pub fn build_index(&mut self) {
        self.by_entity = self.units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.entity, i))
            .collect();
    }

    /// O(1) lookup when index is populated; O(n) linear fallback otherwise.
    pub fn unit(&self, entity: Entity) -> Option<&UnitAiCache> {
        if !self.by_entity.is_empty() {
            let idx = *self.by_entity.get(&entity)?;
            return self.units.get(idx);
        }
        self.units.iter().find(|u| u.entity == entity)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod cache_parity_tests {
    use crate::game::components::Team;
    
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::game::hex::hex_from_offset;

    /// Verify that every field in `AiCache` matches its counterpart in
    /// `UnitSnapshot` for a freshly built snapshot.
    ///
    /// This is the canary for Phase C→D: any divergence here means the cache
    /// population logic is wrong.
    #[test]
    fn cache_fields_match_unit_snapshot_for_all_units() {
        let u1 = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .threat(8.0)
            .max_attack_range(3)
            .build();
        let u2 = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .threat(5.0)
            .max_attack_range(1)
            .build();

        let entities = [u1.entity, u2.entity];
        let snap = snapshot_from(vec![u1, u2], 1);

        for entity in entities {
            let u = snap.unit(entity).expect("unit in snapshot");
            let uc = snap.cache.unit(entity).expect("unit in cache");

            assert_eq!(uc.entity, u.entity(), "entity mismatch");
            assert_eq!(uc.role, u.cache.role, "role mismatch for {:?}", entity);
            assert_eq!(uc.threat, u.cache.threat, "threat mismatch for {:?}", entity);
            assert_eq!(uc.tags, u.cache.tags, "tags mismatch for {:?}", entity);
            assert_eq!(uc.max_attack_range, u.cache.max_attack_range,
                "max_attack_range mismatch for {:?}", entity);
            assert_eq!(uc.aoo_expected_damage, u.cache.aoo_expected_damage,
                "aoo_expected_damage mismatch for {:?}", entity);
            assert_eq!(uc.damage_horizon, u.cache.damage_horizon,
                "damage_horizon mismatch for {:?}", entity);
            assert_eq!(uc.crit_fail_effect, u.cache.crit_fail_effect,
                "crit_fail_effect mismatch for {:?}", entity);
            assert_eq!(uc.ai_tuning_override, u.cache.ai_tuning_override,
                "ai_tuning_override mismatch for {:?}", entity);
            assert_eq!(uc.abilities, u.cache.abilities,
                "abilities mismatch for {:?}", entity);
            assert_eq!(uc.forced_mode, u.cache.forced_mode,
                "forced_mode mismatch for {:?}", entity);
        }
    }
}
