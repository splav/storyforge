use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::scoring::{applies_cc, estimate_damage_horizon, estimate_st_damage};
use crate::combat::ai::world::cache::{AiCache, UnitAiCache};
use crate::combat::ai::world::tags::StatusTagSet;
use crate::combat::ai::world::tags::{AiTags, StatusTagCache};
use crate::combat::bridge::UnitIdMap;
use crate::content::abilities::{AoEShape, CasterContext, EffectDef, TargetType};
use crate::content::content_view::ActiveContentData;
use crate::game::components::{Abilities, AiCombatantQ, Combatant, StatusEffects, Team};
use crate::game::hex::Hex;
use crate::game::hex_map::HexMap;
use bevy::prelude::*;
use combat_engine::ResourceKind;
use std::collections::HashMap;

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BattleSnapshot {
    /// AI-derived per-unit metrics. Populated at `build_snapshot` time;
    /// read by scoring/intent (Phase C+). Source of truth for AI cache data.
    /// Schema: absent in pre-Phase-B logs → `Default` (empty cache).
    #[serde(default)]
    pub cache: AiCache,
    /// Authoritative engine state for this snapshot round.
    /// Added in Phase D-step-2; populated from `CombatStateRes` at build time.
    /// Use `unit(e)` to get a `UnitView` combining both halves.
    #[serde(default)]
    pub state: combat_engine::state::CombatState,
    /// UnitId → Entity translation. Single source of truth for crossing the
    /// engine ↔ ECS namespace boundary. Replaces the broken `from_bits` shortcut
    /// (synthetic UIDs for summons are not valid Entity bits and panic).
    /// Serde-skipped: rebuilt by `rebuild_index` after deserialization.
    #[serde(skip)]
    uid_to_entity: HashMap<combat_engine::state::UnitId, Entity>,
    /// Entity → UnitId inverse of `uid_to_entity`. Needed by `unit(entity)` to
    /// resolve summons whose synthetic UnitIds are not `entity.to_bits()`.
    /// Serde-skipped: rebuilt alongside `uid_to_entity`.
    #[serde(skip)]
    entity_to_uid: HashMap<Entity, combat_engine::state::UnitId>,
}

/// Wire format for `BattleSnapshot`. Mirrors the on-disk representation.
/// Used by the custom `Deserialize` impl to rebuild derived caches after
/// loading — the same pattern `CombatState` uses for its index.
///
/// v38+: only `cache` and `state` are serialized. Older logs that carried
/// `units`/`round` fields are silently ignored by serde (unknown keys).
#[derive(serde::Deserialize)]
struct BattleSnapshotRepr {
    #[serde(default)]
    cache: AiCache,
    #[serde(default)]
    state: combat_engine::state::CombatState,
}

impl<'de> serde::Deserialize<'de> for BattleSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let repr = BattleSnapshotRepr::deserialize(d)?;
        let mut snap = BattleSnapshot {
            uid_to_entity: HashMap::new(),
            entity_to_uid: HashMap::new(),
            cache: repr.cache,
            state: repr.state,
        };
        // Rebuild uid_to_entity / entity_to_uid from state + cache.
        snap.rebuild_index();
        Ok(snap)
    }
}

/// Borrowed view of one unit: engine gameplay state + AI-derived metrics.
///
/// Created on demand by `BattleSnapshot::view(e)` (available after D-step-2
/// when `BattleSnapshot` carries `state: CombatState`).
///
/// `Deref` to `&combat_engine::state::Unit` so gameplay-state reads (`u.hp`,
/// `u.pos`, `u.statuses`, etc.) keep working unchanged; AI-derived reads go
/// through `u.cache` (e.g. `u.cache.threat`, `u.cache.tags`).
///
/// Pass by value — it's two references (16 bytes).
#[derive(Clone, Copy)]
pub struct UnitView<'a> {
    pub state: &'a combat_engine::state::Unit,
    pub cache: &'a UnitAiCache,
}

impl<'a> std::ops::Deref for UnitView<'a> {
    type Target = combat_engine::state::Unit;
    fn deref(&self) -> &combat_engine::state::Unit {
        self.state
    }
}

impl<'a> UnitView<'a> {
    /// Bevy `Entity` for this unit. Read directly from `UnitAiCache.entity`,
    /// which carries the real ECS entity registered at `build_snapshot` time.
    /// Avoids the `Entity::from_bits(self.state.id.0)` shortcut that panics on
    /// summons with synthetic UnitIds (see B-prime).
    pub fn entity(&self) -> bevy::prelude::Entity {
        self.cache.entity
    }

    /// `hp > 0`.
    pub fn is_alive(&self) -> bool {
        self.state.hp() > 0
    }

    /// Read-only view of active statuses (engine `ActiveStatus` slice).
    pub fn statuses(&self) -> &[combat_engine::state::ActiveStatus] {
        &self.state.statuses
    }

    /// Effective HP: raw HP plus armor + armor_bonus — the real damage budget
    /// needed to drop this unit.
    pub fn eff_hp(&self) -> i32 {
        self.state.hp() + self.state.runtime.armor + self.state.armor_bonus
    }

    /// Effective max HP, clamped ≥ 1 to protect against division.
    pub fn eff_max_hp(&self) -> i32 {
        (self.state.max_hp() + self.state.runtime.armor + self.state.armor_bonus).max(1)
    }

    /// Current HP as a fraction of max HP. Clamped max ≥ 1 to avoid div-by-zero.
    pub fn hp_pct(&self) -> f32 {
        self.state.hp() as f32 / self.state.max_hp().max(1) as f32
    }

    /// Killability signal: `1 − eff_hp / eff_max_hp`. 1.0 = dead, 0.0 = full.
    pub fn killability(&self) -> f32 {
        let eff_max = self.eff_max_hp() as f32;
        if eff_max <= 0.0 {
            return 0.0;
        }
        1.0 - (self.eff_hp() as f32 / eff_max)
    }

    /// Current amount in the spendable pool for `kind`.
    pub fn resource_amount(&self, kind: combat_engine::ResourceKind) -> i32 {
        use combat_engine::PoolKind;
        match kind {
            combat_engine::ResourceKind::Hp => self.state.hp(),
            combat_engine::ResourceKind::Mana => self.state.pools[PoolKind::Mana]
                .map(|(c, _)| c)
                .unwrap_or(0),
            combat_engine::ResourceKind::Rage => self.state.pools[PoolKind::Rage]
                .map(|(c, _)| c)
                .unwrap_or(0),
            combat_engine::ResourceKind::Energy => self.state.pools[PoolKind::Energy]
                .map(|(c, _)| c)
                .unwrap_or(0),
        }
    }

    /// True iff the unit has enough AP and every resource cost to cast `def`.
    pub fn can_afford(&self, def: &crate::content::abilities::AbilityDef) -> bool {
        let ap = self.state.pools[combat_engine::PoolKind::Ap]
            .map(|(c, _)| c)
            .unwrap_or(0);
        ap >= def.cost_ap
            && def
                .costs
                .iter()
                .all(|c| self.resource_amount(c.resource) >= c.amount)
    }

    /// True if any active status has the `HARD_CC` tag (stun / paralysis / freeze).
    ///
    /// Computed on the fly from current statuses — never stale.
    pub fn is_stunned(&self, status_tags: &StatusTagCache) -> bool {
        self.state
            .statuses
            .iter()
            .any(|s| status_tags.get(&s.id).contains(StatusTagSet::HARD_CC))
    }

    /// True if any active status has the `COMPULSION` tag (taunt-style binding).
    ///
    /// Computed on the fly from current statuses — never stale.
    pub fn forces_targeting(&self, status_tags: &StatusTagCache) -> bool {
        self.state
            .statuses
            .iter()
            .any(|s| status_tags.get(&s.id).contains(StatusTagSet::COMPULSION))
    }

    /// Override evaluation mode for this unit, if set by a boss phase transition.
    /// `None` means normal tactical evaluation applies.
    pub fn forced_mode(&self) -> Option<crate::combat::ai::adapt::EvaluationMode> {
        self.cache.forced_mode
    }
}

/// Low-level resource-pool lookup. The one place that knows the
/// `ResourceKind` match arms; everybody else — `UnitView` methods,
/// `compute_tags` during snapshot construction, scarcity scoring — funnels
/// through this so the four-arm match doesn't replicate across the crate.
pub(crate) fn pool_amount(kind: ResourceKind, hp: i32, mana: i32, rage: i32, energy: i32) -> i32 {
    match kind {
        ResourceKind::Hp => hp,
        ResourceKind::Mana => mana,
        ResourceKind::Rage => rage,
        ResourceKind::Energy => energy,
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)] // ECS query bundle; splitting into a struct adds churn without clarity
pub fn build_snapshot(
    _round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    hex_map: &HexMap,
    roles: &Query<&AxisProfile>,
    content: &ActiveContentData,
    difficulty: &DifficultyProfile,
    mut combat_state: combat_engine::state::CombatState,
    id_map: &UnitIdMap,
    keep_alive_entities: &std::collections::HashSet<Entity>,
    ai_team: combat_engine::state::Team,
) -> BattleSnapshot {
    // AI visibility filter: only env objects visible to the AI's team enter the
    // snapshot. Hidden enemy traps are absent so AI cannot "cheat" by simulating
    // outcomes it has no knowledge of.
    combat_state.environment.retain(|e| e.visible_to(ai_team));

    let horizon_rounds = difficulty.damage_horizon_rounds;
    // Build AiCache directly from ECS components.
    // Dead combatants are included (hp=0 marker); accessors like `enemies_of`
    // / `allies_of` filter them out; death-aware code reads via `all_enemies_of`
    // / `dead_units`.
    let ai_units: Vec<UnitAiCache> = combatants
        .iter()
        .filter_map(|c| {
            // Guard: entity must exist in at least one spatial layer.
            // Dead units live in HexCorpses — position_of checks both layers so
            // they are not silently dropped from the snapshot (they serve as hp=0
            // markers for death-aware AI accessors like `dead_units`).
            let _pos = hex_map.position_of(c.entity)?;
            let role = roles.get(c.entity).copied().unwrap_or_default();
            let empty_abilities = Abilities::default();
            let abilities: &Abilities = c.abilities.unwrap_or(&empty_abilities);
            let caster_ctx = match c.stats {
                Some(s) => CasterContext::new(s, c.equipment, &content.weapons),
                None => CasterContext::default(),
            };
            let threat = estimate_st_damage(&caster_ctx, abilities, content);

            let mut tags = compute_tags(&c, statuses_q, content);

            // AiTags::OPPONENT_OBJECTIVE: set when the opponent has a KeepAlive
            // victory condition on this unit. Killing it ends combat via defeat
            // for the opponent side — the AI should prioritize it highly.
            if keep_alive_entities.contains(&c.entity) {
                tags |= AiTags::OPPONENT_OBJECTIVE;
            }

            let max_attack_range: u32 = abilities
                .0
                .iter()
                .filter_map(|id| content.abilities.get(id))
                .filter(|def| {
                    matches!(
                        def.target_type,
                        TargetType::SingleEnemy | TargetType::Ground
                    )
                })
                .map(|def| def.range.max)
                .max()
                .unwrap_or(0);

            let has_melee_weapon_attack = abilities.0.iter().any(|id| {
                content.abilities.get(id).is_some_and(|def| {
                    matches!(def.effect, EffectDef::WeaponAttack { ranged: false, .. })
                        && def.range.max == 1
                })
            });
            let aoo_expected_damage = if has_melee_weapon_attack {
                caster_ctx
                    .weapon_dice
                    .as_ref()
                    .map(|d| d.expected() + caster_ctx.str_mod as f32)
            } else {
                None
            };

            let damage_horizon = estimate_damage_horizon(
                &caster_ctx,
                abilities,
                content,
                c.ap.map_or(1, |a| a.max_ap),
                c.mana.map(|m| (m.current, m.max)),
                c.rage.map(|r| (r.current, r.max)),
                c.energy.map(|e| (e.current, e.max)),
                c.vital.hp,
                horizon_rounds,
            );

            let crit_fail_effect = c
                .combat_path
                .and_then(|cp| content.paths.get(&cp.0))
                .map(|p| p.crit_fail_effect.clone())
                .unwrap_or_default();

            // Map AiBehaviorOverride ECS component to EvaluationMode.
            let forced_mode = c.ai_behavior_override.map(|b| {
                use crate::combat::ai::adapt::EvaluationMode;
                use crate::content::encounters::AiBehaviorKind;
                match b.kind {
                    AiBehaviorKind::Flee => EvaluationMode::Flee,
                }
            });

            Some(UnitAiCache {
                entity: c.entity,
                role,
                threat,
                tags,
                max_attack_range,
                aoo_expected_damage,
                damage_horizon,
                crit_fail_effect,
                // TODO(step 2.7): read from a Bevy component once the first
                // unit quirk is introduced. For now, always None — see
                // UnitTemplateDef.ai_tuning_override and ai_rework_plan.md §2.7.
                ai_tuning_override: None,
                abilities: abilities.0.clone(),
                caster_ctx: caster_ctx.clone(),
                forced_mode,
            })
        })
        .collect();

    let mut cache = AiCache::from_units(ai_units);

    // Precompute per-trap severity for the AI team's visible environment objects.
    // The neutral reference is unit-independent, so one cached value per EnvId
    // is valid for all consumers in this decision cycle (T7).
    let (neutral_ref_u, neutral_ref_c) =
        crate::combat::ai::scoring::policy::env_severity::neutral_reference_pair();
    let neutral_ref = UnitView {
        state: &neutral_ref_u,
        cache: &neutral_ref_c,
    };
    for env_obj in &combat_state.environment {
        let sev = crate::combat::ai::scoring::policy::env_severity::severity(
            &env_obj.ability,
            content,
            neutral_ref,
        );
        cache.env_severity.insert(env_obj.id, sev);
    }

    // Build uid_to_entity from id_map — the single namespace-safe translation.
    // Works for both regular units (UnitId == entity.to_bits()) and summons
    // (synthetic UnitId allocated by engine; entity allocated separately by bridge).
    let uid_to_entity: HashMap<combat_engine::state::UnitId, Entity> = cache
        .units
        .iter()
        .filter_map(|c| {
            let uid = id_map.get_id(c.entity)?;
            Some((uid, c.entity))
        })
        .collect();
    // Build entity_to_uid as the inverse — needed by snap.unit(entity) to
    // resolve summons whose synthetic UnitIds are not entity.to_bits().
    let entity_to_uid: HashMap<Entity, combat_engine::state::UnitId> = uid_to_entity
        .iter()
        .map(|(&uid, &entity)| (entity, uid))
        .collect();

    BattleSnapshot {
        cache,
        state: combat_state,
        uid_to_entity,
        entity_to_uid,
    }
}

// ── Helpers on BattleSnapshot ─────────────────────────────────────────────────

impl BattleSnapshot {
    /// Build a snapshot directly from engine `CombatState` + `AiCache`.
    /// Authoritative path — `state` is the engine source of truth, `cache`
    /// holds AI-derived metrics.
    pub fn new(state: combat_engine::state::CombatState, cache: AiCache) -> Self {
        use combat_engine::state::UnitId;
        // Derive uid_to_entity: using the non-summon shortcut
        // (UnitId == entity.to_bits()) that is valid for regular units and for
        // test/replay paths where summons are absent. Build it from cache so
        // we can cross-reference state.
        let uid_to_entity: HashMap<UnitId, Entity> = cache
            .units
            .iter()
            .filter_map(|c| {
                let uid = UnitId(c.entity.to_bits());
                // Only include if the engine state actually has this unit.
                state.unit(uid).map(|_| (uid, c.entity))
            })
            .collect();
        // Inverse map: entity → UnitId (shortcut valid for non-summon callers).
        // SHORTCUT: valid for test/replay/legacy paths where summons are absent.
        // For production paths with summons, `build_snapshot` derives this from
        // `id_map` (the authoritative source).
        let entity_to_uid: HashMap<Entity, UnitId> = uid_to_entity
            .iter()
            .map(|(&uid, &entity)| (entity, uid))
            .collect();
        Self {
            cache,
            state,
            uid_to_entity,
            entity_to_uid,
        }
    }

    /// Rebuild derived caches from `state` + `cache` after deserialization.
    ///
    /// Rebuilds:
    /// - `uid_to_entity`: UnitId→Entity map from cache cross-referenced with state
    /// - `entity_to_uid`: inverse of `uid_to_entity`
    ///
    /// Call after deserialization. No-op when state is empty (nothing to index).
    pub fn rebuild_index(&mut self) {
        // Rebuild uid_to_entity from state cross-referenced with cache.
        // Uses the non-summon shortcut (UnitId == entity.to_bits()) which is
        // valid for deserialized logs (replay/tests) where summons are absent.
        if self.uid_to_entity.is_empty() && !self.state.units().is_empty() {
            use combat_engine::state::UnitId;
            self.uid_to_entity = self
                .cache
                .units
                .iter()
                .filter_map(|c| {
                    let uid = UnitId(c.entity.to_bits());
                    self.state.unit(uid).map(|_| (uid, c.entity))
                })
                .collect();
        }
        // Rebuild entity_to_uid as the inverse of uid_to_entity.
        // Always re-derive to stay in sync even if uid_to_entity was just built.
        if self.entity_to_uid.is_empty() && !self.uid_to_entity.is_empty() {
            self.entity_to_uid = self
                .uid_to_entity
                .iter()
                .map(|(&uid, &entity)| (entity, uid))
                .collect();
        }
    }

    /// Lookup by entity — returns a `UnitView` combining engine state + AI cache.
    /// O(1) when the entity_to_uid map is populated; returns `None` if entity
    /// is unknown to either the engine state or the AI cache.
    ///
    /// Correctly handles summoned units whose synthetic UnitId is not equal to
    /// `entity.to_bits()` — uses `entity_to_uid` for the namespace crossing.
    pub fn unit(&self, entity: Entity) -> Option<UnitView<'_>> {
        let uid = *self.entity_to_uid.get(&entity)?;
        let state = self.state.unit(uid)?;
        let cache = self.cache.unit(entity)?;
        Some(UnitView { state, cache })
    }

    /// Translate engine `UnitId` to Bevy `Entity` via the explicit map.
    /// Use this instead of `Entity::from_bits(uid.0)` — the shortcut panics
    /// for summons whose synthetic UnitIds are not valid Entity bits (B-prime).
    pub fn entity_for_uid(&self, uid: combat_engine::state::UnitId) -> Option<Entity> {
        self.uid_to_entity.get(&uid).copied()
    }

    /// Translate Bevy `Entity` to engine `UnitId` via the explicit map.
    /// Symmetric inverse of `entity_for_uid`. Correctly handles summons whose
    /// synthetic UnitId is not equal to `entity.to_bits()` (B-prime).
    pub fn uid_for_entity(&self, entity: Entity) -> Option<combat_engine::state::UnitId> {
        self.entity_to_uid.get(&entity).copied()
    }

    /// Build a `BattleSnapshot` with an explicit `entity ↔ uid` mapping for use
    /// in tests that need to verify summon handling without a full ECS world.
    ///
    /// Accepts a `(entity, uid)` slice that is used to seed both `uid_to_entity`
    /// and `entity_to_uid`, bypassing the `entity.to_bits()` shortcut used by
    /// `BattleSnapshot::new`. All other maps are derived as usual.
    ///
    /// Not intended for production use — exists so integration tests (in `tests/`)
    /// can exercise the summon lookup path without a full ECS world + `build_snapshot`.
    #[doc(hidden)]
    pub fn new_with_id_map(
        state: combat_engine::state::CombatState,
        cache: crate::combat::ai::world::cache::AiCache,
        id_pairs: &[(Entity, combat_engine::state::UnitId)],
    ) -> Self {
        use combat_engine::state::UnitId;
        let uid_to_entity: HashMap<UnitId, Entity> =
            id_pairs.iter().map(|&(e, uid)| (uid, e)).collect();
        let entity_to_uid: HashMap<Entity, UnitId> =
            id_pairs.iter().map(|&(e, uid)| (e, uid)).collect();
        Self {
            cache,
            state,
            uid_to_entity,
            entity_to_uid,
        }
    }

    /// Position lookup — returns the `UnitView` for the unit at `pos` (if any).
    pub fn unit_at(&self, pos: Hex) -> Option<UnitView<'_>> {
        self.state
            .units()
            .iter()
            .find(|u| u.pos == pos)
            .and_then(|u| {
                let entity = *self.uid_to_entity.get(&u.id)?;
                let cache = self.cache.unit(entity)?;
                Some(UnitView { state: u, cache })
            })
    }

    /// Live enemies of `team` as `UnitView`s. Dead units on the opposing
    /// team are filtered out.
    pub fn enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        let opponent = opponent_team(team);
        self.state.units().iter().filter_map(move |u| {
            if u.team != opponent || u.hp() <= 0 {
                return None;
            }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Live allies of `team` (mirrors `enemies_of` contract).
    pub fn allies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        self.state.units().iter().filter_map(move |u| {
            if u.team != team || u.hp() <= 0 {
                return None;
            }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Enemies of `team` **including corpses**.
    pub fn all_enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        let opponent = opponent_team(team);
        self.state.units().iter().filter_map(move |u| {
            if u.team != opponent {
                return None;
            }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Dead opposing-team units only.
    pub fn dead_enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        let opponent = opponent_team(team);
        self.state.units().iter().filter_map(move |u| {
            if u.team != opponent || u.hp() > 0 {
                return None;
            }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Every dead unit in the snapshot regardless of team.
    pub fn dead_units(&self) -> impl Iterator<Item = UnitView<'_>> {
        self.state.units().iter().filter_map(|u| {
            if u.hp() > 0 {
                return None;
            }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }
}

pub(crate) fn opponent_team(team: Team) -> Team {
    match team {
        Team::Player => Team::Enemy,
        Team::Enemy => Team::Player,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

use crate::game::components::AiCombatantQItem;

fn compute_tags(
    c: &AiCombatantQItem,
    _statuses_q: &Query<&StatusEffects>,
    content: &ActiveContentData,
) -> AiTags {
    let mut tags = AiTags::empty();

    // LOW_HP: below 30%
    let hp_pct = c.vital.hp as f32 / c.vital.max_hp.max(1) as f32;
    if hp_pct < 0.3 {
        tags |= AiTags::LOW_HP;
    }

    // Ability-derived tags
    let mut max_range: u32 = 0;
    let mut has_min_range_2 = false;

    let resources = (
        c.mana.map(|m| m.current).unwrap_or(0),
        c.rage.map(|r| r.current).unwrap_or(0),
        c.energy.map(|e| e.current).unwrap_or(0),
    );

    let empty = Abilities::default();
    let abilities = c.abilities.unwrap_or(&empty);
    for id in &abilities.0 {
        let Some(def) = content.abilities.get(id) else {
            continue;
        };
        if def.range.max > max_range {
            max_range = def.range.max;
        }
        if def.range.min >= 2 {
            has_min_range_2 = true;
        }

        let can_afford = def.costs.iter().all(|cost| {
            pool_amount(
                cost.resource,
                c.vital.hp,
                resources.0,
                resources.1,
                resources.2,
            ) >= cost.amount
        });

        if can_afford {
            if def.target_type == TargetType::SingleAlly
                && matches!(def.effect, EffectDef::Heal { .. })
            {
                tags |= AiTags::CAN_HEAL;
            }

            if applies_cc(def, content) {
                tags |= AiTags::CAN_CC;
            }

            if def.aoe != AoEShape::None {
                tags |= AiTags::HAS_AOE;
            }
        }
    }

    if max_range <= 1 {
        tags |= AiTags::MELEE_ONLY;
    }
    if has_min_range_2 {
        tags |= AiTags::RANGED;
    }

    tags
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
