//! Shared test helpers for the `ai/` tree. Lives in the binary only under
//! `cfg(test)` — provides the common `UtilityContext` / `UnitSnapshot` /
//! `InfluenceMaps` / `ContentView` scaffolding that every scoring-adjacent
//! test module used to hand-roll.

use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
use crate::combat::ai::role::{AiRole, AxisProfile};
use crate::combat::ai::snapshot::{AiTags, UnitSnapshot};
use crate::combat::ai::utility::{ActorCtx, AiWorld, UtilityContext};
use crate::content::abilities::CasterContext;
use crate::content::content_view::ContentView;
use crate::content::races::CritFailEffect;
use crate::core::AbilityId;
use crate::game::components::{Abilities, Team};
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use std::collections::HashMap;

// ── Utility context ────────────────────────────────────────────────────────

/// Build a `UtilityContext` with the conventional test defaults
/// (`crit_fail_effect: Miss`, `crit_fail_chance: 0.0`). Caller supplies the
/// caster so per-suite str/int/spell-power tweaks stay explicit.
pub(crate) fn make_test_ctx<'a>(
    content: &'a ContentView,
    difficulty: &'a DifficultyProfile,
    caster: &'a CasterContext,
    abilities: &'a Abilities,
) -> UtilityContext<'a> {
    UtilityContext {
        world: AiWorld { content, difficulty },
        actor: ActorCtx {
            caster,
            abilities,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        },
    }
}

// ── Unit snapshot builder ──────────────────────────────────────────────────

/// Fluent builder for `UnitSnapshot` test fixtures. Replaces the 10 copies
/// of `fn unit(...)` that previously hand-rolled 24-field struct literals
/// with slightly-different defaults in each test module. Call sites override
/// only the fields that matter for their scenario (`.hp(5).tags(LOW_HP)`).
pub(crate) struct UnitBuilder {
    inner: UnitSnapshot,
}

#[allow(dead_code)] // full chain kept for future tests; used ones rotate.
impl UnitBuilder {
    /// Reasonable "generic melee bruiser" defaults. Tests override via the
    /// chain methods below. Canonical defaults (picked to match the most
    /// common old factory): hp/max_hp=20, ap=1/max=1, speed=3, mp=3,
    /// threat=5.0, max_attack_range=1, role=Bruiser, tags=empty.
    pub fn new(id: u32, team: Team, pos: Hex) -> Self {
        Self {
            inner: UnitSnapshot {
                entity: Entity::from_raw_u32(id).expect("valid entity id"),
                team,
                role: AxisProfile::from(AiRole::Bruiser),
                pos,
                hp: 20,
                max_hp: 20,
                armor: 0,
                armor_bonus: 0,
                damage_taken_bonus: 0,
                action_points: 1,
                max_ap: 1,
                movement_points: 3,
                speed: 3,
                mana: None,
                rage: None,
                energy: None,
                abilities: Vec::new(),
                threat: 5.0,
                tags: AiTags::empty(),
                max_attack_range: 1,
                summoner: None,
                reactions_left: 0,
                aoo_expected_damage: None,
                statuses: Vec::new(),
            },
        }
    }

    pub fn hp(mut self, hp: i32) -> Self {
        self.inner.hp = hp;
        self
    }
    pub fn max_hp(mut self, max_hp: i32) -> Self {
        self.inner.max_hp = max_hp;
        self
    }
    pub fn full_hp(mut self, hp: i32) -> Self {
        self.inner.hp = hp;
        self.inner.max_hp = hp;
        self
    }
    pub fn armor(mut self, armor: i32) -> Self {
        self.inner.armor = armor;
        self
    }
    pub fn ap(mut self, ap: i32) -> Self {
        self.inner.action_points = ap;
        self.inner.max_ap = ap;
        self
    }
    pub fn speed(mut self, speed: i32) -> Self {
        self.inner.speed = speed;
        self.inner.movement_points = speed;
        self
    }
    pub fn threat(mut self, threat: f32) -> Self {
        self.inner.threat = threat;
        self
    }
    pub fn role(mut self, role: AxisProfile) -> Self {
        self.inner.role = role;
        self
    }
    pub fn ai_role(self, role: AiRole) -> Self {
        self.role(AxisProfile::from(role))
    }
    pub fn tags(mut self, tags: AiTags) -> Self {
        self.inner.tags = tags;
        self
    }
    pub fn abilities(mut self, ids: Vec<AbilityId>) -> Self {
        self.inner.abilities = ids;
        self
    }
    pub fn ability_names(self, names: &[&str]) -> Self {
        self.abilities(names.iter().map(|s| AbilityId::from(*s)).collect())
    }
    pub fn max_attack_range(mut self, r: u32) -> Self {
        self.inner.max_attack_range = r;
        self
    }
    pub fn mana(mut self, current: i32, max: i32) -> Self {
        self.inner.mana = Some((current, max));
        self
    }
    pub fn rage(mut self, current: i32, max: i32) -> Self {
        self.inner.rage = Some((current, max));
        self
    }
    pub fn energy(mut self, current: i32, max: i32) -> Self {
        self.inner.energy = Some((current, max));
        self
    }
    pub fn summoner(mut self, entity: Entity) -> Self {
        self.inner.summoner = Some(entity);
        self
    }
    pub fn aoo(mut self, expected_damage: f32, reactions: i32) -> Self {
        self.inner.aoo_expected_damage = Some(expected_damage);
        self.inner.reactions_left = reactions;
        self
    }
    pub fn build(self) -> UnitSnapshot {
        self.inner
    }
}

/// Short-hand for `UnitBuilder::new(id, team, pos).build()` — the dominant
/// single-line fixture shape across test modules.
pub(crate) fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
    UnitBuilder::new(id, team, pos).build()
}

/// Convenience for making an `Entity` from a raw u32 test id.
pub(crate) fn ent(id: u32) -> Entity {
    Entity::from_raw_u32(id).expect("valid entity id")
}

// ── Influence maps ─────────────────────────────────────────────────────────

/// All four influence maps empty (zero danger / ally_support / opportunity /
/// escape). Test bodies add specific tiles via `maps.danger.add(...)` when
/// they care.
pub(crate) fn empty_maps() -> InfluenceMaps {
    InfluenceMaps {
        danger: InfluenceMap::new(),
        ally_support: InfluenceMap::new(),
        opportunity: InfluenceMap::new(),
        escape: InfluenceMap::new(),
    }
}

// ── Content ────────────────────────────────────────────────────────────────

/// Completely empty `ContentView` — every registry is a new HashMap /
/// Vec. Tests that need a specific ability/status insert it after
/// construction.
pub(crate) fn empty_content() -> ContentView {
    ContentView {
        abilities: HashMap::new(),
        keyed_abilities: Vec::new(),
        statuses: HashMap::new(),
        weapons: HashMap::new(),
        armor: HashMap::new(),
        classes: HashMap::new(),
        unit_templates: HashMap::new(),
        races: HashMap::new(),
        factions: HashMap::new(),
        paths: HashMap::new(),
    }
}
