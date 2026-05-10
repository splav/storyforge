use crate::content::content_view::ContentView;
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::scoring::{applies_cc, estimate_damage_horizon, estimate_st_damage};
use crate::combat::ai::config::tuning::AiTuningOverride;
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectDef, TargetType};
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, ResourceKind, StatusId};
use crate::game::components::{
    AiCombatantQ, Combatant, StatusEffects, Team,
};
use crate::game::hex::Hex;
use crate::game::resources::HexPositions;
use crate::combat::ai::world::tags::{AiTags, StatusTagCache};
use crate::combat::ai::world::tags::cache::StatusBonuses;
use crate::combat::ai::world::tags::StatusTagSet;
use bevy::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BattleSnapshot {
    pub units: Vec<UnitSnapshot>,
    pub round: u32,
    /// O(1) entity → units[index] cache. Private so the invariant "in sync
    /// with `units`" can't be silently broken via struct-literal
    /// construction; callers go through [`BattleSnapshot::new`] or the
    /// serde path (which gives a `None` cache, lazy-built on first
    /// `unit()` call). Sim calls [`BattleSnapshot::invalidate_index`] after
    /// `units.retain` / other shape-changing mutations so the next read
    /// rebuilds. Read through `unit()` — never poke this field directly.
    #[serde(skip)]
    by_entity: RefCell<Option<HashMap<Entity, usize>>>,
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

    /// Recompute all derived fields (`speed`, `armor_bonus`, `damage_taken_bonus`,
    /// and the `IS_STUNNED` / `FORCES_TARGETING` tag bits) from `base_speed` +
    /// active statuses.
    ///
    /// Numeric bonuses are summed over every active status via the cache;
    /// `IS_STUNNED` is set iff any active status has `HARD_CC` in the cache,
    /// `FORCES_TARGETING` iff any has `COMPULSION`. All other `AiTags` bits
    /// (`LOW_HP`, `MELEE_ONLY`, `RANGED`, `CAN_CC`, `CAN_HEAL`, `HAS_AOE`)
    /// are not status-derived and are left untouched.
    pub fn refresh_aggregates(&mut self, status_tags: &StatusTagCache) {
        let mut speed_bonus: i32 = 0;
        let mut armor_bonus: i32 = 0;
        let mut damage_taken_bonus: i32 = 0;
        let mut is_stunned = false;
        let mut forces_targeting = false;

        for s in &self.statuses {
            let bonuses = status_tags.bonuses(&s.id);
            speed_bonus += bonuses.speed_bonus;
            armor_bonus += bonuses.armor_bonus;
            damage_taken_bonus += bonuses.damage_taken_bonus;

            let tags = status_tags.get(&s.id);
            if tags.contains(StatusTagSet::HARD_CC) {
                is_stunned = true;
            }
            if tags.contains(StatusTagSet::COMPULSION) {
                forces_targeting = true;
            }
        }

        self.speed = self.base_speed + speed_bonus;
        self.armor_bonus = armor_bonus;
        self.damage_taken_bonus = damage_taken_bonus;

        // Clear only the status-derived tag bits, then re-set them.
        self.tags.remove(AiTags::IS_STUNNED | AiTags::FORCES_TARGETING);
        if is_stunned {
            self.tags |= AiTags::IS_STUNNED;
        }
        if forces_targeting {
            self.tags |= AiTags::FORCES_TARGETING;
        }
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
    round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    positions: &HexPositions,
    roles: &Query<&AxisProfile>,
    content: &ContentView,
    difficulty: &DifficultyProfile,
) -> BattleSnapshot {
    let horizon_rounds = difficulty.damage_horizon_rounds;
    // Dead combatants stay in the snapshot (hp=0 marker). Downstream
    // accessors like `enemies_of` / `allies_of` filter them out; death-
    // aware code (resurrection, on-kill triggers, replay) reads them via
    // `all_enemies_of` / `dead_units`.
    let units = combatants
        .iter()
        .filter_map(|c| {
            let pos = positions.get(&c.entity)?;
            let role = roles.get(c.entity).copied().unwrap_or_default();
            let caster_ctx = CasterContext::new(c.stats, Some(c.equipment), &content.weapons);
            let threat = estimate_st_damage(&caster_ctx, c.abilities, content);

            let tags = compute_tags(&c, statuses_q, content);

            // Single pass over status effects — aggregates every per-snapshot
            // bonus at once (speed, armor, damage-taken). Keep this fold as
            // the only place each bonus is read from statuses.
            let StatusBonuses { speed_bonus, armor_bonus, damage_taken_bonus } =
                status_bonuses(c.entity, statuses_q, content);

            let max_attack_range: u32 = c
                .abilities
                .0
                .iter()
                .filter_map(|id| content.abilities.get(id))
                // Ground-targeted abilities also project "attack reach":
                // a mage with fireball (Ground, range 5) should be treated
                // as having a 5-tile threat bubble, just like SingleEnemy.
                .filter(|def| matches!(
                    def.target_type,
                    TargetType::SingleEnemy | TargetType::Ground
                ))
                .map(|def| def.range.max)
                .max()
                .unwrap_or(0);

            // AoO provoker data: has melee weapon_attack + equipped weapon.
            // Mirrors `movement.rs` provoker selection.
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
            let reactions_left = c.reactions.map(|r| r.remaining as i32).unwrap_or(0);

            let statuses = statuses_q
                .get(c.entity)
                .map(|se| {
                    se.0.iter()
                        .map(|s| ActiveStatusView {
                            id: s.id.clone(),
                            rounds_remaining: s.rounds_remaining,
                            dot_per_tick: s.dot_per_tick,
                        })
                        .collect()
                })
                .unwrap_or_default();

            Some(UnitSnapshot {
                entity: c.entity,
                team: c.faction.0,
                role,
                pos,
                hp: c.vital.hp,
                max_hp: c.vital.max_hp,
                armor: c.vital.armor,
                armor_bonus,
                damage_taken_bonus,
                action_points: c.ap.action_points,
                max_ap: c.ap.max_ap,
                movement_points: c.ap.movement_points,
                base_speed: c.speed.0,
                speed: c.speed.0 + speed_bonus,
                mana: c.mana.map(|m| (m.current, m.max)),
                rage: c.rage.map(|r| (r.current, r.max)),
                energy: c.energy.map(|e| (e.current, e.max)),
                abilities: c.abilities.0.clone(),
                threat,
                tags,
                max_attack_range,
                summoner: c.summoned_by.map(|s| s.0),
                reactions_left,
                aoo_expected_damage,
                statuses,
                // `caster_ctx` is already built above for threat/AoO; reuse it
                // so readers get the same derivation without recomputing.
                caster_ctx: caster_ctx.clone(),
                crit_fail_effect: c
                    .combat_path
                    .and_then(|cp| content.paths.get(&cp.0))
                    .map(|p| p.crit_fail_effect.clone())
                    .unwrap_or_default(),
                damage_horizon: estimate_damage_horizon(
                    &caster_ctx,
                    c.abilities,
                    content,
                    c.ap.max_ap,
                    c.mana.map(|m| (m.current, m.max)),
                    c.rage.map(|r| (r.current, r.max)),
                    c.energy.map(|e| (e.current, e.max)),
                    c.vital.hp,
                    horizon_rounds,
                ),
                // TODO(step 2.7): read from a Bevy component once the first
                // unit quirk is introduced. For now, always None — see
                // UnitTemplateDef.ai_tuning_override and ai_rework_plan.md §2.7.
                ai_tuning_override: None,
            })
        })
        .collect();

    BattleSnapshot::new(units, round)
}

// ── Helpers on BattleSnapshot ─────────────────────────────────────────────────

impl BattleSnapshot {
    /// Construct a snapshot with its entity index eagerly built. The index
    /// backs `unit()` — O(1) instead of the linear scan the old literal
    /// construction produced. Use this everywhere (prod + tests);
    /// `#[derive(Default)]` / serde-deserialized snapshots get an empty
    /// cache that lazy-builds on first `unit()` call.
    pub fn new(units: Vec<UnitSnapshot>, round: u32) -> Self {
        let map = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.entity, i))
            .collect();
        Self {
            units,
            round,
            by_entity: RefCell::new(Some(map)),
        }
    }

    /// Discard the entity index. Call after any mutation of `units` that
    /// changes length or order (e.g. sim's `retain` for killed units). The
    /// next `unit()` call rebuilds lazily.
    pub fn invalidate_index(&mut self) {
        *self.by_entity.borrow_mut() = None;
    }

    /// O(1) lookup by entity. Transparently lazy-builds the index when
    /// missing (fresh deserialized snapshot or post-`invalidate_index`).
    pub fn unit(&self, entity: Entity) -> Option<&UnitSnapshot> {
        // Scope the RefCell borrow so the resulting `&UnitSnapshot` doesn't
        // alias it.
        let idx = {
            let mut slot = self.by_entity.borrow_mut();
            if slot.is_none() {
                *slot = Some(
                    self.units
                        .iter()
                        .enumerate()
                        .map(|(i, u)| (u.entity, i))
                        .collect(),
                );
            }
            slot.as_ref().expect("just filled").get(&entity).copied()?
        };
        self.units.get(idx)
    }

    /// Position lookup stays linear — there's no per-tile index, and
    /// callers use it sparingly (mostly in sim's `compute_affected_targets`).
    pub fn unit_at(&self, pos: Hex) -> Option<&UnitSnapshot> {
        self.units.iter().find(|u| u.pos == pos)
    }

    /// Live enemies of `team`. Dead units on the opposing team stay in
    /// `units` (kept for resurrection / death triggers / replay fidelity)
    /// but are filtered here because every tactical caller wants the
    /// "who can I actually fight" view. For the raw list including
    /// corpses, use `all_enemies_of`.
    pub fn enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units
            .iter()
            .filter(move |u| u.team == opponent && u.is_alive())
    }

    /// Live allies of `team` (mirrors `enemies_of` contract).
    pub fn allies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        self.units
            .iter()
            .filter(move |u| u.team == team && u.is_alive())
    }

    /// Enemies of `team` **including corpses**. Used by death-aware code
    /// (resurrection targeting, on-kill effect resolution, replay) that
    /// needs to see the entity even after it died.
    pub fn all_enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units.iter().filter(move |u| u.team == opponent)
    }

    /// Dead opposing-team units only. Empty on live-only snapshots; the
    /// resurrection/death-trigger call sites poll this.
    pub fn dead_enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units
            .iter()
            .filter(move |u| u.team == opponent && !u.is_alive())
    }

    /// Every dead unit in the snapshot regardless of team.
    pub fn dead_units(&self) -> impl Iterator<Item = &UnitSnapshot> {
        self.units.iter().filter(|u| !u.is_alive())
    }
}

fn opponent_team(team: Team) -> Team {
    match team {
        Team::Player => Team::Enemy,
        Team::Enemy => Team::Player,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

use crate::game::components::AiCombatantQItem;

fn compute_tags(
    c: &AiCombatantQItem,
    statuses_q: &Query<&StatusEffects>,
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

    // Status-derived tags
    if let Ok(se) = statuses_q.get(c.entity) {
        for s in &se.0 {
            let Some(sd) = content.statuses.get(&s.id) else { continue };
            if sd.skips_turn {
                tags |= AiTags::IS_STUNNED;
            }
            if sd.forces_targeting {
                tags |= AiTags::FORCES_TARGETING;
            }
        }
    }

    tags
}

/// Aggregate every status-derived bonus a snapshot needs in a single pass over
/// the unit's `StatusEffects`. Before this helper we iterated the status list
/// three times per unit (once per bonus field).
fn status_bonuses(
    entity: Entity,
    statuses_q: &Query<&StatusEffects>,
    content: &ContentView,
) -> StatusBonuses {
    let Ok(se) = statuses_q.get(entity) else {
        return StatusBonuses::default();
    };
    se.0.iter()
        .filter_map(|s| content.statuses.get(&s.id))
        .fold(StatusBonuses::default(), |mut acc, sd| {
            acc.speed_bonus += sd.speed_bonus;
            acc.armor_bonus += sd.armor_bonus;
            acc.damage_taken_bonus += sd.damage_taken_bonus;
            acc
        })
}

#[cfg(test)]
mod affordability_tests {
    use super::*;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef, ResourceCost};
    use crate::core::DiceExpr;
    use crate::game::hex::hex_from_offset;

    fn base_unit() -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(1).expect("valid"),
            team: Team::Enemy,
            role: AxisProfile { tank: 0.5, melee: 0.5, ..Default::default() },
            pos: hex_from_offset(0, 0),
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 2,
            max_ap: 2,
            movement_points: 3,
            base_speed: 3,
            speed: 3,
            mana: Some((5, 10)),
            rage: Some((3, 10)),
            energy: Some((4, 10)),
            abilities: Vec::new(),
            threat: 0.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 1,
            aoo_expected_damage: None,
            statuses: Vec::new(),
            caster_ctx: Default::default(),
            crit_fail_effect: Default::default(),
            damage_horizon: Vec::new(),
            ai_tuning_override: None,
        }
    }

    fn def(cost_ap: i32, costs: Vec<ResourceCost>) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("x"),
            name: "x".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs,
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn cost(kind: ResourceKind, amount: i32) -> ResourceCost {
        ResourceCost { resource: kind, amount }
    }

    #[test]
    fn can_afford_covers_ap_and_all_resource_kinds() {
        let u = base_unit();
        // (name, ap_cost, costs, expected can_afford)
        let cases: Vec<(&str, i32, Vec<ResourceCost>, bool)> = vec![
            ("free ability",        1, vec![],                              true),
            ("AP shortage",         3, vec![],                              false),
            ("mana ok",             1, vec![cost(ResourceKind::Mana, 5)],   true),
            ("mana short",          1, vec![cost(ResourceKind::Mana, 6)],   false),
            ("rage ok",             1, vec![cost(ResourceKind::Rage, 3)],   true),
            ("rage short",          1, vec![cost(ResourceKind::Rage, 4)],   false),
            ("energy ok",           1, vec![cost(ResourceKind::Energy, 4)], true),
            ("energy short",        1, vec![cost(ResourceKind::Energy, 5)], false),
            ("hp ok",               1, vec![cost(ResourceKind::Hp, 20)],    true),
            ("hp short",            1, vec![cost(ResourceKind::Hp, 21)],    false),
            ("two costs both ok",   1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 3)], true),
            ("two costs one short", 1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 4)], false),
        ];
        for (name, ap_cost, costs, want) in cases {
            let d = def(ap_cost, costs);
            assert_eq!(u.can_afford(&d), want, "{name}");
        }
    }

    #[test]
    fn resource_amount_treats_absent_option_pools_as_zero() {
        let mut u = base_unit();
        u.mana = None;
        u.rage = None;
        u.energy = None;
        assert_eq!(u.resource_amount(ResourceKind::Mana), 0);
        assert_eq!(u.resource_amount(ResourceKind::Rage), 0);
        assert_eq!(u.resource_amount(ResourceKind::Energy), 0);
        assert_eq!(u.resource_amount(ResourceKind::Hp), u.hp);
        // Any positive cost on an absent pool fails.
        let d = def(1, vec![cost(ResourceKind::Mana, 1)]);
        assert!(!u.can_afford(&d));
    }

    /// Dead units stay in `units` (hp=0 marker); the default-facing
    /// `enemies_of` / `allies_of` accessors hide them, while the explicit
    /// `all_enemies_of` / `dead_units` surface them for resurrection / on-kill /
    /// replay call sites. Pins the new contract.
    #[test]
    fn dead_units_stay_in_snapshot_and_are_filtered_by_default() {
        let alive = base_unit();
        let mut corpse = base_unit();
        corpse.entity = Entity::from_raw_u32(2).expect("valid");
        corpse.team = Team::Player;
        corpse.hp = 0;
        let snap = BattleSnapshot::new(vec![alive.clone(), corpse.clone()], 1);

        assert!(snap.unit(corpse.entity).is_some(), "corpse must stay in units");
        assert_eq!(
            snap.unit(corpse.entity).map(|u| u.is_alive()),
            Some(false),
            "corpse must report is_alive = false",
        );

        // Default accessors hide the dead.
        assert_eq!(snap.enemies_of(Team::Enemy).count(), 0, "default enemies_of hides dead");
        assert_eq!(snap.allies_of(Team::Enemy).count(), 1, "alive ally still visible");

        // Explicit "all" + "dead" variants surface them.
        assert_eq!(snap.all_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_units().count(), 1);
    }
}

#[cfg(test)]
mod snapshot_api_tests {
    use super::*;
    use crate::combat::ai::test_helpers::{empty_status_tag_cache, UnitBuilder};
    use crate::game::hex::hex_from_offset;
    use crate::game::components::Team;

    fn test_unit() -> UnitSnapshot {
        UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build()
    }

    fn test_status(id: &str) -> ActiveStatusView {
        ActiveStatusView {
            id: StatusId::from(id),
            rounds_remaining: 2,
            dot_per_tick: 0,
        }
    }

    // ── base_speed ────────────────────────────────────────────────────────────

    /// v35 logs lack `base_speed` — deserialise as 0 via `#[serde(default)]`.
    #[test]
    fn base_speed_default_zero_on_v35_deserialise() {
        // Serialize a current unit, then strip `base_speed` to simulate a v35 log.
        let unit = test_unit();
        let json = serde_json::to_string(&unit).expect("serialize");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        value.as_object_mut().unwrap().remove("base_speed");
        let json_v35 = serde_json::to_string(&value).unwrap();

        let restored: UnitSnapshot = serde_json::from_str(&json_v35).expect("deserialise v35 snapshot");
        assert_eq!(restored.base_speed, 0, "base_speed absent in v35 JSON → deserialises as 0");
        assert_eq!(restored.speed, unit.speed);
    }

    /// base_speed round-trips through JSON (v36+ schema where field is present).
    #[test]
    fn base_speed_serialized_on_round_trip() {
        let mut unit = test_unit();
        unit.base_speed = 3;
        let json = serde_json::to_string(&unit).expect("serialize");
        let restored: UnitSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.base_speed, 3);
    }

    // ── add_status / remove_status / statuses() ───────────────────────────────

    #[test]
    fn add_status_inserts_and_calls_refresh() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        assert_eq!(unit.statuses().len(), 0);
        unit.add_status(test_status("foo"), cache);
        assert_eq!(unit.statuses().len(), 1);
        assert_eq!(unit.statuses()[0].id, StatusId::from("foo"));
    }

    #[test]
    fn remove_status_returns_true_when_removed_false_when_absent() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        unit.add_status(test_status("foo"), cache);

        assert!(unit.remove_status(&StatusId::from("foo"), cache), "should return true for existing status");
        assert!(!unit.remove_status(&StatusId::from("nonexistent"), cache), "should return false for absent status");
        assert!(unit.statuses().is_empty(), "no statuses remain");
    }

    #[test]
    fn statuses_accessor_returns_immutable_slice() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        unit.add_status(test_status("bar"), cache);
        let slice: &[ActiveStatusView] = unit.statuses();
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0].id, StatusId::from("bar"));
    }

    // ── refresh_aggregates: speed ─────────────────────────────────────────────

    /// Build a minimal `StatusTagCache` containing a single status with the
    /// given tags and bonuses. Used by refresh_aggregates tests to avoid
    /// needing a full `ContentView` load.
    fn cache_with_status(id: &str, tags: StatusTagSet, bonuses: StatusBonuses) -> StatusTagCache {
        let mut c = StatusTagCache::default();
        let sid = StatusId::from(id);
        c.map.insert(sid.clone(), tags);
        c.bonuses.insert(sid, bonuses);
        c
    }

    #[test]
    fn apply_haste_increases_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "haste",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("haste"), &cache);
        assert_eq!(unit.speed, 5, "base 3 + speed_bonus 2 = 5");
    }

    #[test]
    fn apply_slow_decreases_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "slow",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: -1, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("slow"), &cache);
        assert_eq!(unit.speed, 2, "base 3 + speed_bonus -1 = 2");
    }

    #[test]
    fn expire_haste_restores_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "haste",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("haste"), &cache);
        assert_eq!(unit.speed, 5);
        unit.remove_status(&StatusId::from("haste"), &cache);
        assert_eq!(unit.speed, 3, "after removing haste speed returns to base 3");
    }

    #[test]
    fn multiple_speed_statuses_stack() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let mut cache = StatusTagCache::default();
        let haste_id = StatusId::from("haste");
        let bless_id = StatusId::from("bless");
        cache.map.insert(haste_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(haste_id.clone(), StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 });
        cache.map.insert(bless_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(bless_id.clone(), StatusBonuses { speed_bonus: 1, armor_bonus: 0, damage_taken_bonus: 0 });

        unit.add_status(test_status("haste"), &cache);
        unit.add_status(test_status("bless"), &cache);
        assert_eq!(unit.speed, 6, "base 3 + haste(+2) + bless(+1) = 6");
    }

    #[test]
    fn apply_armor_buff_recomputes_armor_bonus() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "stone_skin",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 0, armor_bonus: 5, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("stone_skin"), &cache);
        assert_eq!(unit.armor_bonus, 5);
    }

    #[test]
    fn apply_vulnerability_recomputes_damage_taken_bonus() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "vuln",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 3 },
        );
        unit.add_status(test_status("vuln"), &cache);
        assert_eq!(unit.damage_taken_bonus, 3);
    }

    #[test]
    fn hard_cc_status_sets_is_stunned_tag() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "stun",
            StatusTagSet::HARD_CC,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("stun"), &cache);
        assert!(unit.tags.contains(AiTags::IS_STUNNED), "HARD_CC status must set IS_STUNNED");

        unit.remove_status(&StatusId::from("stun"), &cache);
        assert!(!unit.tags.contains(AiTags::IS_STUNNED), "removing stun must clear IS_STUNNED");
    }

    #[test]
    fn compulsion_status_sets_forces_targeting_tag() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "taunted",
            StatusTagSet::COMPULSION,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("taunted"), &cache);
        assert!(unit.tags.contains(AiTags::FORCES_TARGETING), "COMPULSION status must set FORCES_TARGETING");

        unit.remove_status(&StatusId::from("taunted"), &cache);
        assert!(!unit.tags.contains(AiTags::FORCES_TARGETING), "removing taunt must clear FORCES_TARGETING");
    }

    #[test]
    fn refresh_preserves_non_status_tags() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .tags(AiTags::LOW_HP | AiTags::MELEE_ONLY)
            .build();
        let cache = cache_with_status(
            "stun",
            StatusTagSet::HARD_CC,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("stun"), &cache);

        // IS_STUNNED added by refresh_aggregates
        assert!(unit.tags.contains(AiTags::IS_STUNNED));
        // Non-status-derived bits must be untouched
        assert!(unit.tags.contains(AiTags::LOW_HP), "LOW_HP must survive refresh");
        assert!(unit.tags.contains(AiTags::MELEE_ONLY), "MELEE_ONLY must survive refresh");
    }
}
