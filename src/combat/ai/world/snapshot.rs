use crate::combat::ai::world::cache::{AiCache, UnitAiCache};
use crate::content::content_view::ContentView;
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::scoring::{applies_cc, estimate_damage_horizon, estimate_st_damage};
use crate::combat::ai::config::tuning::AiTuningOverride;
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectDef, TargetType};
use crate::content::races::CritFailEffect;
use combat_engine::{AbilityId, ResourceKind, StatusId};
use crate::game::components::{
    AiCombatantQ, Combatant, StatusEffects, Team,
};
use crate::game::hex::Hex;
use crate::game::resources::HexPositions;
use crate::combat::ai::world::tags::{AiTags, StatusTagCache};
#[cfg(test)]
use crate::combat::ai::world::tags::cache::StatusBonuses;
use crate::combat::ai::world::tags::StatusTagSet;
use crate::combat::engine_bridge::UnitIdMap;
use bevy::prelude::*;
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UnitSnapshot {
    #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
    pub entity: Entity,
    pub team: Team,
    pub role: AxisProfile,
    #[serde(with = "crate::combat::ai::log::serde_helpers::hex")]
    pub pos: Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    /// Remaining AP for this turn.
    pub action_points: i32,
    /// Full AP pool (for partial-spend reasoning).
    pub max_ap: i32,
    /// Movement budget remaining right now. Zero means the unit can't walk.
    pub movement_points: i32,
    /// Base move budget without status modifiers. Always equals
    /// `speed - sum(active speed bonuses from statuses)`.
    /// Schema v36+ (bumped in step 12.4): serialized explicitly.
    /// v35 logs deserialise as 0 via `#[serde(default)]`; post-load
    /// reconstructor in `parse_actor_tick` sets `base_speed = speed`
    /// (safe assumption: speed-bonus statuses very rare in v35 corpus).
    #[serde(default)]
    pub base_speed: i32,
    /// Base speed + status speed_bonus. Used for pathfinding range estimates
    /// and utility scoring; not the live budget (see `movement_points`).
    pub speed: i32,
    pub mana: Option<(i32, i32)>,
    pub rage: Option<(i32, i32)>,
    pub energy: Option<(i32, i32)>,
    pub abilities: Vec<AbilityId>,
    pub threat: f32,
    #[serde(with = "crate::combat::ai::log::serde_helpers::ai_tags")]
    pub tags: AiTags,
    /// Max range of any offensive (SingleEnemy) ability. Used for reach checks
    /// in intent selection (e.g., "is this enemy killable this turn?").
    pub max_attack_range: u32,
    /// Entity of the summoner, if this unit was summoned.
    #[serde(with = "crate::combat::ai::log::serde_helpers::entity_opt")]
    pub summoner: Option<Entity>,
    /// Remaining opportunity reactions this round. Zero means no AoO this turn.
    /// (Schema v2+: default `1` on v1 logs — every current unit has `max=1`.)
    #[serde(default = "default_reactions_left")]
    pub reactions_left: i32,
    /// Pre-armor expected damage this unit would inflict via an AoO
    /// (dice.expected + str_mod). `None` if the unit cannot make an opportunity
    /// attack (no melee weapon_attack ability, or no equipped weapon).
    /// Schema v2+: absent on v1 logs → `None`.
    #[serde(default)]
    pub aoo_expected_damage: Option<f32>,
    /// Active status effects on this unit — mirrors the `StatusEffects`
    /// component (minus the `applier` Entity, which isn't needed for AI
    /// reasoning). Sim mutates this list on per-step status applications so
    /// that downstream steps see status-derived bonuses / DoT cleanse / stun.
    /// Schema v3+: absent on older logs → empty vec.
    ///
    /// **Convention**: lib code MUST mutate via `add_status` / `remove_status`
    /// so that `refresh_aggregates` is called atomically. Field stays `pub`
    /// because external bin crates (mining, replay) construct test fixtures
    /// via struct literal — `pub(crate)` would block that. Invariant safety
    /// inside lib is a convention enforced by code review, not the type system.
    #[serde(default)]
    pub statuses: Vec<ActiveStatusView>,
    /// Caster parameters (str/int mod, spell power, weapon dice). Derived
    /// from stats + equipment at snapshot time; kept here so every scoring
    /// call site reads the actor's caster data from the same source as its
    /// HP/AP/abilities (one entity ⇒ one row).
    /// Schema v3+: absent on older logs → `CasterContext::default()`.
    #[serde(default)]
    pub caster_ctx: CasterContext,
    /// Actor's crit-fail behaviour (from the combat path definition). Lives
    /// on the snapshot so scoring doesn't need a separate per-actor context;
    /// pairs naturally with `caster_ctx` — both are facts about "this
    /// entity's combat shape" at snapshot time.
    /// Schema v3+: absent on older logs → `CritFailEffect::Miss`.
    #[serde(default)]
    pub crit_fail_effect: CritFailEffect,
    /// Projected damage per future round under AP + resource budgets, as
    /// produced by `estimate_damage_horizon`. `damage_horizon[i]` is the
    /// expected single-target damage this unit deals `i+1` rounds from
    /// now. Length matches `DifficultyProfile.damage_horizon_rounds`
    /// (typically 5). Sum over a relevant duration window captures "how
    /// much damage this unit is projected to deliver while a stun / heal
    /// window is in effect" — DPR-correct where plain `threat` over-counts
    /// resource-limited burst casters.
    ///
    /// Schema v4+: absent on older logs → empty vec; CC/heal scoring
    /// reading horizon falls back to `threat`-only behaviour when empty.
    #[serde(default)]
    pub damage_horizon: Vec<f32>,
    /// Per-actor AiTuning override, propagated from the unit's template
    /// (`ai_tuning_override` in unit_templates.toml). `None` for units without
    /// a quirk — which is every unit in the current content, see step 2.7 of
    /// ai_rework_plan.md. Consumed once in `pick_action` via
    /// `AiTuning::apply_override`.
    ///
    /// Schema v18+: absent on v≤17 logs → `None`.
    // TODO(step 2.7): wire UnitTemplateDef.ai_tuning_override → a Bevy component
    // → read it here in build_snapshot when the first quirk is introduced.
    #[serde(default)]
    pub ai_tuning_override: Option<AiTuningOverride>,
}

/// Snapshot-shaped mirror of `ActiveStatus` (components.rs). Drops `applier`
/// since the AI layer never needs to know who put the status on — only the
/// status id, duration, and per-tick DoT damage are consulted.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ActiveStatusView {
    pub id: StatusId,
    pub rounds_remaining: u32,
    pub dot_per_tick: i32,
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
        self.state.hp > 0
    }

    /// Read-only view of active statuses (engine `ActiveStatus` slice).
    pub fn statuses(&self) -> &[combat_engine::state::ActiveStatus] {
        &self.state.statuses
    }

    /// Effective HP: raw HP plus armor + armor_bonus — the real damage budget
    /// needed to drop this unit.
    pub fn eff_hp(&self) -> i32 {
        self.state.hp + self.state.armor + self.state.armor_bonus
    }

    /// Effective max HP, clamped ≥ 1 to protect against division.
    pub fn eff_max_hp(&self) -> i32 {
        (self.state.max_hp + self.state.armor + self.state.armor_bonus).max(1)
    }

    /// Current HP as a fraction of max HP. Clamped max ≥ 1 to avoid div-by-zero.
    pub fn hp_pct(&self) -> f32 {
        self.state.hp as f32 / self.state.max_hp.max(1) as f32
    }

    /// Killability signal: `1 − eff_hp / eff_max_hp`. 1.0 = dead, 0.0 = full.
    pub fn killability(&self) -> f32 {
        let eff_max = self.eff_max_hp() as f32;
        if eff_max <= 0.0 { return 0.0; }
        1.0 - (self.eff_hp() as f32 / eff_max)
    }

    /// Current amount in the spendable pool for `kind`.
    pub fn resource_amount(&self, kind: combat_engine::ResourceKind) -> i32 {
        pool_amount(
            kind,
            self.state.hp,
            self.state.mana.map(|(c, _)| c).unwrap_or(0),
            self.state.rage.map(|(c, _)| c).unwrap_or(0),
            self.state.energy.map(|(c, _)| c).unwrap_or(0),
        )
    }

    /// True iff the unit has enough AP and every resource cost to cast `def`.
    pub fn can_afford(&self, def: &crate::content::abilities::AbilityDef) -> bool {
        self.state.action_points >= def.cost_ap
            && def.costs.iter().all(|c| self.resource_amount(c.resource) >= c.amount)
    }

    /// True if any active status has the `HARD_CC` tag (stun / paralysis / freeze).
    ///
    /// Computed on the fly from current statuses — never stale.
    pub fn is_stunned(&self, status_tags: &StatusTagCache) -> bool {
        self.state.statuses.iter().any(|s| {
            status_tags.get(&s.id).contains(StatusTagSet::HARD_CC)
        })
    }

    /// True if any active status has the `COMPULSION` tag (taunt-style binding).
    ///
    /// Computed on the fly from current statuses — never stale.
    pub fn forces_targeting(&self, status_tags: &StatusTagCache) -> bool {
        self.state.statuses.iter().any(|s| {
            status_tags.get(&s.id).contains(StatusTagSet::COMPULSION)
        })
    }
}

fn default_reactions_left() -> i32 { 1 }


impl UnitSnapshot {
    /// `hp > 0`. Snapshot keeps dead units (for death-triggered effects,
    /// resurrection, and honest replay logs); accessors like `enemies_of`
    /// filter them out by default. Use this directly when a call site needs
    /// both "alive?" and "pos occupied?" (the classic case — movement
    /// stop-blockers — counts corpses even though they're not enemies).
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Effective HP: raw HP plus base and status armor — the real damage
    /// budget needed to drop this unit.
    pub fn eff_hp(&self) -> i32 {
        self.hp + self.armor + self.armor_bonus
    }

    /// Effective max HP, clamped ≥ 1 to protect against division.
    pub fn eff_max_hp(&self) -> i32 {
        (self.max_hp + self.armor + self.armor_bonus).max(1)
    }

    /// Current HP as a fraction of max HP, clamped ≥ 1 to avoid div-by-zero.
    /// Use for threshold checks like "below 30% HP triggers LOW_HP".
    pub fn hp_pct(&self) -> f32 {
        self.hp as f32 / self.max_hp.max(1) as f32
    }

    /// Killability signal: `1 − eff_hp / eff_max_hp`. A 1.0 unit is dead,
    /// 0.0 is at full effective HP. Used by `target_priority` (scoring the
    /// focus factor) and by the planner's target enumeration (picking the
    /// top-K most-finishable enemies for Cast candidates).
    pub fn killability(&self) -> f32 {
        let eff_max = self.eff_max_hp() as f32;
        if eff_max <= 0.0 {
            return 0.0;
        }
        1.0 - (self.eff_hp() as f32 / eff_max)
    }

    /// Current amount in the spendable pool for `kind`. `Option` resources
    /// (mana/rage/energy) yield 0 when absent.
    pub fn resource_amount(&self, kind: ResourceKind) -> i32 {
        pool_amount(
            kind,
            self.hp,
            self.mana.map(|(c, _)| c).unwrap_or(0),
            self.rage.map(|(c, _)| c).unwrap_or(0),
            self.energy.map(|(c, _)| c).unwrap_or(0),
        )
    }

    /// True iff the unit has enough AP and every resource cost to cast `def`.
    pub fn can_afford(&self, def: &AbilityDef) -> bool {
        self.action_points >= def.cost_ap
            && def
                .costs
                .iter()
                .all(|c| self.resource_amount(c.resource) >= c.amount)
    }

    // ── Status access API ─────────────────────────────────────────────────────

    /// Read-only view of active statuses.
    pub fn statuses(&self) -> &[ActiveStatusView] {
        &self.statuses
    }

    /// Add a status and atomically refresh derived aggregates.
    pub fn add_status(&mut self, status: ActiveStatusView, status_tags: &crate::combat::ai::world::tags::StatusTagCache) {
        self.statuses.push(status);
        self.refresh_aggregates(status_tags);
    }

    /// Remove a status by id and atomically refresh derived aggregates.
    /// Returns `true` if the status was present and removed.
    pub fn remove_status(&mut self, id: &StatusId, status_tags: &crate::combat::ai::world::tags::StatusTagCache) -> bool {
        let before = self.statuses.len();
        self.statuses.retain(|s| &s.id != id);
        let changed = self.statuses.len() != before;
        if changed {
            self.refresh_aggregates(status_tags);
        }
        changed
    }

    /// Raw mutable access to the statuses list for bulk operations (bulk-remove,
    /// retain, tick). **Caller MUST call `refresh_aggregates` after mutating.**
    #[allow(dead_code)]
    pub(crate) fn statuses_mut(&mut self) -> &mut Vec<ActiveStatusView> {
        &mut self.statuses
    }

    /// Recompute numeric derived fields (`speed`, `armor_bonus`,
    /// `damage_taken_bonus`) from `base_speed` + active statuses.
    ///
    /// Numeric bonuses are summed over every active status via the cache.
    /// All `AiTags` bits are left untouched — `IS_STUNNED` and
    /// `FORCES_TARGETING` have been removed from the bitfield; use
    /// `UnitView::is_stunned` / `UnitView::forces_targeting` to test those
    /// conditions on the live status list instead.
    pub fn refresh_aggregates(&mut self, status_tags: &StatusTagCache) {
        let mut speed_bonus: i32 = 0;
        let mut armor_bonus: i32 = 0;
        let mut damage_taken_bonus: i32 = 0;

        for s in &self.statuses {
            let bonuses = status_tags.bonuses(&s.id);
            speed_bonus += bonuses.speed_bonus;
            armor_bonus += bonuses.armor_bonus;
            damage_taken_bonus += bonuses.damage_taken_bonus;
        }

        self.speed = self.base_speed + speed_bonus;
        self.armor_bonus = armor_bonus;
        self.damage_taken_bonus = damage_taken_bonus;
    }

    /// True if any active status has the `HARD_CC` tag.
    ///
    /// Shim for callers that hold a `&UnitSnapshot` (deprecated path /
    /// test fixtures). Prefer `UnitView::is_stunned` in production code.
    pub fn is_stunned(&self, status_tags: &StatusTagCache) -> bool {
        self.statuses.iter().any(|s| {
            status_tags.get(&s.id).contains(StatusTagSet::HARD_CC)
        })
    }

    /// True if any active status has the `COMPULSION` tag.
    ///
    /// Shim for callers that hold a `&UnitSnapshot` (deprecated path /
    /// test fixtures). Prefer `UnitView::forces_targeting` in production code.
    pub fn forces_targeting(&self, status_tags: &StatusTagCache) -> bool {
        self.statuses.iter().any(|s| {
            status_tags.get(&s.id).contains(StatusTagSet::COMPULSION)
        })
    }

}

/// Low-level resource-pool lookup. The one place that knows the
/// `ResourceKind` match arms; everybody else — `UnitSnapshot` methods,
/// `compute_tags` during snapshot construction, scarcity scoring — funnels
/// through this so the four-arm match doesn't replicate across the crate.
pub(crate) fn pool_amount(
    kind: ResourceKind,
    hp: i32,
    mana: i32,
    rage: i32,
    energy: i32,
) -> i32 {
    match kind {
        ResourceKind::Hp => hp,
        ResourceKind::Mana => mana,
        ResourceKind::Rage => rage,
        ResourceKind::Energy => energy,
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub fn build_snapshot(
    _round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    positions: &HexPositions,
    roles: &Query<&AxisProfile>,
    content: &ContentView,
    difficulty: &DifficultyProfile,
    combat_state: combat_engine::state::CombatState,
    id_map: &UnitIdMap,
) -> BattleSnapshot {
    let horizon_rounds = difficulty.damage_horizon_rounds;
    // Build AiCache directly from ECS components.
    // Dead combatants are included (hp=0 marker); accessors like `enemies_of`
    // / `allies_of` filter them out; death-aware code reads via `all_enemies_of`
    // / `dead_units`.
    let ai_units: Vec<UnitAiCache> = combatants
        .iter()
        .filter_map(|c| {
            let _pos = positions.get(&c.entity)?;
            let role = roles.get(c.entity).copied().unwrap_or_default();
            let caster_ctx = CasterContext::new(c.stats, Some(c.equipment), &content.weapons);
            let threat = estimate_st_damage(&caster_ctx, c.abilities, content);

            let tags = compute_tags(&c, statuses_q, content);

            let max_attack_range: u32 = c
                .abilities
                .0
                .iter()
                .filter_map(|id| content.abilities.get(id))
                .filter(|def| matches!(
                    def.target_type,
                    TargetType::SingleEnemy | TargetType::Ground
                ))
                .map(|def| def.range.max)
                .max()
                .unwrap_or(0);

            let has_melee_weapon_attack = c.abilities.0.iter().any(|id| {
                content.abilities.get(id).is_some_and(|def| {
                    matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
                })
            });
            let aoo_expected_damage =
                if has_melee_weapon_attack {
                    caster_ctx
                        .weapon_dice
                        .as_ref()
                        .map(|d| d.expected() + caster_ctx.str_mod as f32)
                } else {
                    None
                };

            let damage_horizon = estimate_damage_horizon(
                &caster_ctx,
                c.abilities,
                content,
                c.ap.max_ap,
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

            Some(UnitAiCache {
                entity:              c.entity,
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
                abilities:           c.abilities.0.clone(),
                caster_ctx:          caster_ctx.clone(),
            })
        })
        .collect();

    let cache = AiCache::from_units(ai_units);

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
    let entity_to_uid: HashMap<Entity, combat_engine::state::UnitId> =
        uid_to_entity.iter().map(|(&uid, &entity)| (entity, uid)).collect();

    BattleSnapshot { cache, state: combat_state, uid_to_entity, entity_to_uid }
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
        let entity_to_uid: HashMap<Entity, UnitId> =
            uid_to_entity.iter().map(|(&uid, &entity)| (entity, uid)).collect();
        Self { cache, state, uid_to_entity, entity_to_uid }
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
            self.uid_to_entity = self.cache.units.iter().filter_map(|c| {
                let uid = UnitId(c.entity.to_bits());
                self.state.unit(uid).map(|_| (uid, c.entity))
            }).collect();
        }
        // Rebuild entity_to_uid as the inverse of uid_to_entity.
        // Always re-derive to stay in sync even if uid_to_entity was just built.
        if self.entity_to_uid.is_empty() && !self.uid_to_entity.is_empty() {
            self.entity_to_uid = self.uid_to_entity
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
        Self { cache, state, uid_to_entity, entity_to_uid }
    }

    

    /// Position lookup — returns the `UnitView` for the unit at `pos` (if any).
    pub fn unit_at(&self, pos: Hex) -> Option<UnitView<'_>> {
        self.state.units().iter().find(|u| u.pos == pos).and_then(|u| {
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
            if u.team != opponent || u.hp <= 0 { return None; }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Live allies of `team` (mirrors `enemies_of` contract).
    pub fn allies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        self.state.units().iter().filter_map(move |u| {
            if u.team != team || u.hp <= 0 { return None; }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Enemies of `team` **including corpses**.
    pub fn all_enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        let opponent = opponent_team(team);
        self.state.units().iter().filter_map(move |u| {
            if u.team != opponent { return None; }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Dead opposing-team units only.
    pub fn dead_enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        let opponent = opponent_team(team);
        self.state.units().iter().filter_map(move |u| {
            if u.team != opponent || u.hp > 0 { return None; }
            let entity = *self.uid_to_entity.get(&u.id)?;
            let cache = self.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Every dead unit in the snapshot regardless of team.
    pub fn dead_units(&self) -> impl Iterator<Item = UnitView<'_>> {
        self.state.units().iter().filter_map(|u| {
            if u.hp > 0 { return None; }
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
    content: &ContentView,
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

    for id in &c.abilities.0 {
        let Some(def) = content.abilities.get(id) else { continue };
        if def.range.max > max_range {
            max_range = def.range.max;
        }
        if def.range.min >= 2 {
            has_min_range_2 = true;
        }

        let can_afford = def.costs.iter().all(|cost| {
            pool_amount(cost.resource, c.vital.hp, resources.0, resources.1, resources.2)
                >= cost.amount
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
