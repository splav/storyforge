//! Bevy ↔ `combat_engine` boundary.
//!
//! This module is the **only** place that imports both `bevy::` and
//! `combat_engine::`.  The engine itself (`crates/combat_engine/`) has zero
//! Bevy dependency (decision 6.7).
//!
//! # What lives here
//!
//! - `UnitIdMap` — `Res<UnitIdMap>` holding the `Entity ↔ UnitId` mapping.
//! - `from_ecs` — populates a `CombatState` from current ECS components.
//!   One-directional ECS → engine; transitional for Phase 0.
//! - `CombatStateRes` — `Res<CombatStateRes>` wrapping the pure `CombatState`
//!   so the engine state can live in Bevy without the engine importing Bevy.
//! - `bootstrap_combat_state` — system chained at the end of `CombatPhase::StartRound`
//!   (after `build_turn_order`) that initializes `CombatStateRes` once per combat.
//!   Engine state becomes authoritative from combat start; ECS mirrors via projection.
//! - `process_action_system` — `Update` system (Phase 1) that consumes
//!   `ActionInput` messages and calls `combat_engine::step()` as a parallel
//!   witness.  Output is ignored — ECS is still authoritative via
//!   `movement_system`.
//!
//! ## `Entity → UnitId` encoding
//!
//! Uses `Entity::to_bits()` — Bevy's own canonical u64 serialization of an
//! entity (low bits = index, high bits = generation).  Stable within a session;
//! not stable across save/load (generation counters reset).
//!
//! **Flagged for next agent:** save/load stability will require a separate
//! persistent id scheme if needed in Phase 5+.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ecs::system::SystemParam;

use crate::app_state::CombatPhase;
use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::content::races::CritFailEffect;
use crate::combat::ai::config::role::{infer_profile, AxisProfile};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::game::combat_log::{
    CombatEvent, CombatLog, CritFailOutcomeEcs, SpawnBlockedReasonEcs, TurnSkipReasonEcs,
};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AuraSource, BonusMovement, CombatPath, CombatStats,
    Combatant, Dead, Energy, Equipment, EnemyPhases, Faction, Mana, Rage, Reactions, Speed,
    StatusEffects, SummonedBy, UnitToken, Vital,
};
use crate::game::bundles::enemy_bundle;
use crate::game::hex::LAYOUT;
use crate::game::messages::{ActionInput, RestartCombat};
use crate::game::resources::{CombatContext, HexCorpses, HexPositions, TurnQueue};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

use combat_engine::{
    action::Action,
    content::{AuraDef, ContentView as EngineContentView, TeamRelation},
    event::Event,
    reaction::ReactionKind,
    state::{ActiveStatus, CombatState, Pool, RoundPhase, Unit, UnitId},
    step::step,
};
use combat_engine::dice::DiceExpr as EngineDiceExpr;
use combat_engine::modifier;

// ── Entity ↔ UnitId mapping ───────────────────────────────────────────────────

/// Bidirectional `Entity ↔ UnitId` map stored as a Bevy resource.
///
/// Rebuilt at combat start by `from_ecs`.  Stable for the duration of a session.
#[derive(Resource, Default)]
pub struct UnitIdMap {
    pub entity_to_id: HashMap<Entity, UnitId>,
    pub id_to_entity: HashMap<UnitId, Entity>,
}

impl UnitIdMap {
    /// Reset and rebuild. Called at the top of `from_ecs`.
    pub fn clear(&mut self) {
        self.entity_to_id.clear();
        self.id_to_entity.clear();
    }

    /// Insert one mapping.  Debug-panics on duplicate entity.
    pub fn insert(&mut self, entity: Entity, id: UnitId) {
        debug_assert!(
            !self.entity_to_id.contains_key(&entity),
            "UnitIdMap: entity {entity:?} already mapped"
        );
        self.entity_to_id.insert(entity, id);
        self.id_to_entity.insert(id, entity);
    }

    pub fn get_id(&self, entity: Entity) -> Option<UnitId> {
        self.entity_to_id.get(&entity).copied()
    }

    pub fn get_entity(&self, id: UnitId) -> Option<Entity> {
        self.id_to_entity.get(&id).copied()
    }
}

// ── Entity → UnitId encoding ──────────────────────────────────────────────────

/// Encode a Bevy `Entity` as a `u64` for `UnitId`.
///
/// Uses `Entity::to_bits()` which is Bevy's canonical stable serialization
/// encoding (low bits = index, high bits = generation).
pub fn entity_to_uid(entity: Entity) -> UnitId {
    UnitId(entity.to_bits())
}

// ── CombatStateRes ────────────────────────────────────────────────────────────

/// Bevy resource wrapper for the pure `CombatState`.
///
/// Exists solely so `CombatState` (which lives in `combat_engine/` with zero
/// Bevy imports) can be stored as a Bevy `Res`.  The inner state is initialized
/// once per round by `init_state_from_ecs` on `OnEnter(CombatPhase::AwaitCommand)`.
///
/// **Phase 0 transitional:** engine writes go nowhere; ECS is still
/// authoritative.  Phase 1+ will reverse this.
#[derive(Resource, Default)]
pub struct CombatStateRes(pub CombatState);

// ── CombatState::from_ecs ────────────────────────────────────────────────────

/// Query type alias for readability.
///
/// Deadness is read off `Vital.hp <= 0`, not `Has<Dead>` — matches the
/// projector's convention so both directions agree on a single predicate.
type CombatantRow<'a> = (
    Entity,
    &'a Vital,
    &'a Speed,
    &'a ActionPoints,
    &'a Reactions,
    &'a Faction,
    Option<&'a StatusEffects>,
    Option<&'a Rage>,
    Option<&'a Mana>,
    Option<&'a Energy>,
);

/// Populate a `CombatState` from the current ECS world; also rebuilds `id_map`.
///
/// Components read:
/// - `Vital` — hp/max_hp/armor
/// - `Speed` — base speed
/// - `ActionPoints` — ap/movement_points
/// - `Reactions` — reactions_left
/// - `Faction` — team
/// - `StatusEffects` (optional) — active statuses
/// - `Rage` / `Mana` (optional) — resource pools
/// - `HexPositions` resource — alive unit positions (occupancy layer)
/// - `HexCorpses` resource — dead unit positions (corpse layer)
///
/// Dead units (`Has<Dead>`) are kept as tombstones (hp=0), matching the
/// `BattleSnapshot` convention so downstream code can filter by `is_alive()`.
///
/// `content` is used to recompute per-unit aggregates (`armor_bonus`,
/// `speed`, `damage_taken_bonus`) from active statuses and auras, mirroring
/// the `Effect::RefreshAggregates` logic so the engine starts with correct
/// derived values.  Pass `&active_content` from `Res<ActiveContent>`.
pub fn from_ecs(
    combatants: &Query<CombatantRow, With<Combatant>>,
    positions: &HexPositions,
    corpses: &HexCorpses,
    round: u32,
    id_map: &mut UnitIdMap,
    content: &ActiveContent,
) -> CombatState {
    id_map.clear();

    let units: Vec<Unit> = combatants
        .iter()
        .filter_map(|(entity, vital, speed, ap, reactions, faction, statuses, rage, mana, energy_opt)| {
            let is_dead = vital.hp <= 0;
            // Alive units live in HexPositions; dead units in HexCorpses.
            let pos = if is_dead {
                corpses.get(&entity)?
            } else {
                positions.get(&entity)?
            };

            let uid = entity_to_uid(entity);
            id_map.insert(entity, uid);

            let statuses_vec: Vec<ActiveStatus> = statuses
                .map(|se| {
                    se.0.iter()
                        .map(|s| ActiveStatus {
                            id: s.id.clone(),
                            rounds_remaining: s.rounds_remaining,
                            dot_per_tick: s.dot_per_tick,
                            applier: entity_to_uid(s.applier),
                        })
                        .collect()
                })
                .unwrap_or_default();

            let team = faction.0;

            // Dead units: keep with hp=0 (tombstone).
            let hp = if is_dead { 0 } else { vital.hp };

            let rage_pool: Option<Pool> = rage.map(|r| (r.current, r.max));
            let mana_pool: Option<Pool> = mana.map(|m| (m.current, m.max));
            let energy_pool: Option<Pool> = energy_opt.map(|e| (e.current, e.max));

            // Compute status-derived aggregate bonuses from active statuses.
            // Mirrors Effect::RefreshAggregates (status half only); aura-based
            // contributions are added after bootstrap populates unit.auras.
            let mut armor_bonus: i32 = 0;
            let mut speed_bonus: i32 = 0;
            let mut damage_taken_bonus: i32 = 0;
            for s in &statuses_vec {
                if let Some(def) = content.statuses.get(&s.id) {
                    armor_bonus       += def.engine.bonuses.armor_bonus;
                    speed_bonus       += def.engine.bonuses.speed_bonus;
                    damage_taken_bonus += def.engine.bonuses.damage_taken_bonus;
                }
            }

            Some(Unit {
                id: uid,
                team,
                pos,
                hp,
                max_hp: vital.max_hp,
                armor: vital.armor,
                armor_bonus,
                damage_taken_bonus,
                base_speed: speed.0,
                speed: speed.0 + speed_bonus,
                action_points: ap.action_points,
                max_ap: ap.max_ap,
                movement_points: ap.movement_points,
                // Bootstrap-initial: a unit always enters combat with a full reaction
                // budget. We intentionally ignore `Reactions.remaining` here — the ECS
                // default starts at 0 (matching `Effect::Spawn`'s reactions_left=0 for
                // mid-combat summons), so reading it would yield 0 and break round-1
                // AoO. Engine's `start_round` (called on `Effect::BumpRound`) refills
                // reactions_left = reactions_max on every subsequent round.
                reactions_left: reactions.max as i32,
                reactions_max: reactions.max as i32,
                statuses: statuses_vec,
                rage: rage_pool,
                mana: mana_pool,
                energy: energy_pool,
                summoner: None,
                // Per-combat fields populated by bootstrap_combat_state after from_ecs.
                caster_context: combat_engine::CasterContext::default(),
                aoo_dice: None,
                auras: Vec::new(),
                enemy_phases: Vec::new(),
            })
        })
        .collect();

    CombatState::new(units, round, RoundPhase::ActorTurn, 0)
}

// ── process_action_system ─────────────────────────────────────────────────────

/// ECS-backed `ContentView` adapter for `process_action_system`.
///
/// After 5c.1, this struct carries only static content (active_content).
/// Per-combat state (caster contexts, auras, AoO dice, phase triggers) now
/// lives on engine `Unit` fields and is populated once at combat init by
/// `from_ecs` / `bootstrap_combat_state`.
pub struct EcsContentView<'a> {
    active_content: &'a ActiveContent,
}

impl<'a> EngineContentView for EcsContentView<'a> {
    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
        self.active_content.abilities.get(id).map(|a| &a.engine)
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> {
        self.active_content.statuses.get(id).map(|s| &s.engine)
    }

    fn unit_template(&self, id: &str) -> Option<combat_engine::UnitTemplate> {
        let tpl = self.active_content.unit_templates.get(id)?;
        Some(build_engine_template_from_def(tpl, self.active_content))
    }
}

/// Deferred queue of phase transitions to apply at the end of `Execute`.
///
/// `process_action_system` and `bootstrap_combat_state` push `(UnitId, phase_idx)`
/// for each `Event::PhaseEntered` they see.
/// `apply_phase_transitions_system` drains the queue and writes ECS-only deltas
/// (Name, Abilities, AxisProfile, EnemyPhases.pending pop, Dead removal, max_hp).
/// Running as a separate system after `project_state_to_ecs` avoids a Bevy
/// query conflict between the phase-write query and the projector's `&mut Vital`.
#[derive(Resource, Default)]
pub struct PendingPhaseTransitions(pub Vec<(UnitId, usize)>);

/// Build a fully-populated engine `UnitTemplate` from a bridge `UnitTemplateDef`.
///
/// Mirrors the caster_context and aoo_dice logic in `bootstrap_combat_state` but
/// works from content data alone (no live ECS queries).  Used by
/// `EcsContentView::unit_template` so that summon `Effect::Spawn` receives a
/// complete template with correct combat stats.
///
/// `auras` and `enemy_phases` are left empty: `UnitTemplateDef` has no aura/phase
/// fields (MVP — those are encounter-level data, not template-level).
fn build_engine_template_from_def(
    tpl: &crate::content::unit_templates::UnitTemplateDef,
    active_content: &ActiveContent,
) -> combat_engine::UnitTemplate {
    let equipment = Equipment {
        main_hand: Some(tpl.equipment.main_hand.clone()),
        off_hand: tpl.equipment.off_hand.clone(),
        chest: tpl.equipment.chest.clone(),
        legs: tpl.equipment.legs.clone(),
        feet: tpl.equipment.feet.clone(),
    };
    let effective = active_content.effective_stats(&tpl.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);

    // Build CasterContext from stats + main-hand weapon, mirroring CasterContext::new.
    let bevy_ctx = CasterContext::new(&tpl.stats, Some(&equipment), &active_content.weapons);
    // crit_fail_outcome: look up the unit's combat path, default to Miss.
    let crit_fail_effect = tpl.path
        .as_deref()
        .and_then(|p| active_content.paths.get(p))
        .map_or(crate::content::races::CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
    let engine_ctx = combat_engine::CasterContext {
        str_mod: bevy_ctx.str_mod,
        int_mod: bevy_ctx.int_mod,
        spell_power: bevy_ctx.spell_power,
        weapon_dice: bevy_ctx.weapon_dice,
        crit_fail_outcome: crate::content::to_engine::crit_fail_outcome(&crit_fail_effect),
    };

    // AoO dice: unit needs a melee WeaponAttack ability (range.max == 1) + weapon dice.
    let has_melee = tpl.ability_ids.iter().any(|aid| {
        active_content.abilities.get(aid).is_some_and(|def| {
            matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
        })
    });
    let aoo_dice = if has_melee {
        bevy_ctx.weapon_dice.map(|core_dice| {
            EngineDiceExpr::new(
                core_dice.count,
                core_dice.sides,
                core_dice.bonus + combat_engine::modifier(tpl.stats.strength),
            )
        })
    } else {
        None
    };

    combat_engine::UnitTemplate {
        max_hp: effective.max_hp,
        armor,
        base_speed: tpl.speed,
        max_ap: 1, // templates carry no max_ap; matches CombatantBundle hardcoded default
        mana_max: tpl.resources.mana_max,
        energy_max: tpl.resources.energy_max,
        rage_max: tpl.resources.rage_max,
        caster_context: engine_ctx,
        aoo_dice,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    }
}

/// Build `EcsContentView` from the current ECS state.
/// Build `EcsContentView` from the current ECS state.
///
/// After 5c.1, `EcsContentView` only wraps `ActiveContent` — all per-combat
/// state (caster contexts, auras, phase triggers) now lives on engine `Unit`
/// fields and is populated once at init by `from_ecs`.
///
/// Called from `bootstrap_combat_state`, `process_action_system`, and
/// `advance_turn_system` (for dead-actor sirota-DoT ticks).
pub(crate) fn build_ecs_content_view<'a>(
    content: &'a ActiveContent,
) -> EcsContentView<'a> {
    EcsContentView { active_content: content }
}

// ── apply_phase_ecs_writes ────────────────────────────────────────────────────

/// Apply ECS-only deltas for a boss phase transition.
///
/// Called for each `Event::PhaseEntered` seen in a translator event stream.
/// Reproduces the logic of the deleted `phase_transition_system` (4d/4e):
///   1. Reads `EnemyPhases.pending[phase_idx]` for the new Name, Abilities,
///      CombatStats, and flavor text.
///   2. Mutates ECS components: `Name`, `Abilities`, `CombatStats`, `Vital`
///      (re-infers `AxisProfile`; removes `Dead` if `heal_to_full` revived).
///   3. Pops `pending[phase_idx]` (spec §8: exactly one pop per event).
///   4. Pushes `CombatEvent::PhaseEntered` with `prev_name`/`next_name`/`flavor`.
///
/// Called from `apply_phase_transitions_system` which runs AFTER `project_state_to_ecs`
/// to avoid a query conflict over `&mut Vital` between the two systems.
/// `process_action_system` and `bootstrap_combat_state` record `(unit, phase_idx)`
/// pairs into `PendingPhaseTransitions`; this helper drains them.
fn apply_phase_ecs_writes(
    unit: UnitId,
    phase_idx: usize,
    id_map: &UnitIdMap,
    commands: &mut Commands,
    log: &mut CombatLog,
    q: &mut Query<(
        &mut EnemyPhases,
        &mut Vital,
        &mut CombatStats,
        &mut Abilities,
        Option<&mut AxisProfile>,
        &mut Name,
        Has<Dead>,
    )>,
    content: &ActiveContent,
    tag_cache: &AbilityTagCache,
) {
    let Some(ent) = id_map.get_entity(unit) else { return };
    let Ok((mut phases, mut vital, mut stats, mut abilities, role_opt, mut name, is_dead)) =
        q.get_mut(ent)
    else {
        return;
    };

    let Some(phase) = phases.pending.get(phase_idx).cloned() else { return };

    // Capture name before mutation so the log shows the actual "was → now".
    let prev_name = name.as_str().to_string();

    if let Some(new_stats) = &phase.stats {
        *stats = new_stats.clone();
        vital.max_hp = new_stats.max_hp;
        // Clamp current HP to new max; heal_to_full overrides below.
        // project_state_to_ecs writes vital.hp from engine state (which already
        // committed the phase transition), but does NOT write vital.max_hp.
        vital.hp = vital.hp.min(vital.max_hp);
    }
    if phase.heal_to_full {
        vital.hp = vital.max_hp;
    }
    if is_dead && vital.hp > 0 {
        commands.entity(ent).remove::<Dead>();
    }
    if let Some(ref new_ability_ids) = phase.ability_ids {
        abilities.0 = new_ability_ids.clone();
    }
    if let Some(mut role) = role_opt {
        if phase.stats.is_some() || phase.ability_ids.is_some() {
            // Re-infer AxisProfile when the inputs (abilities / max_hp / armor) changed.
            *role = infer_profile(&abilities.0, vital.max_hp, vital.armor, content, tag_cache);
        }
    }

    let next_name = phase.name.clone().unwrap_or_else(|| prev_name.clone());
    if phase.name.is_some() {
        *name = Name::new(next_name.clone());
    }

    log.push(CombatEvent::PhaseEntered {
        actor: ent,
        prev_name,
        next_name,
        flavor: phase.flavor.clone(),
    });

    // Pop exactly once per event (spec §8).
    phases.pending.remove(phase_idx);
}

/// Drain `PendingPhaseTransitions` and write ECS-only phase deltas.
///
/// Runs AFTER `project_state_to_ecs` in the `Execute` step to avoid a Bevy
/// query conflict: `project_state_to_ecs` needs `&mut Vital` for HP projection;
/// phase writes also need `&mut Vital` for `max_hp` and `heal_to_full`.
/// Running as a separate system after the projector resolves the ambiguity.
pub fn apply_phase_transitions_system(
    mut pending: ResMut<PendingPhaseTransitions>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut log: ResMut<CombatLog>,
    active_content: Res<ActiveContent>,
    tag_cache: Res<AbilityTagCache>,
    mut q: Query<(
        &mut EnemyPhases,
        &mut Vital,
        &mut CombatStats,
        &mut Abilities,
        Option<&mut AxisProfile>,
        &mut Name,
        Has<Dead>,
    )>,
) {
    let transitions = std::mem::take(&mut pending.0);
    for (unit, phase_idx) in transitions {
        apply_phase_ecs_writes(unit, phase_idx, &id_map, &mut commands, &mut log, &mut q, &active_content, &tag_cache);
    }
}

// ── VisualAssets / ContentParams SystemParam newtypes ────────────────────────

/// Bundles rendering-only Bevy resources used by `process_action_system`
/// and `spawn_ecs_entity_from_engine_unit`.
///
/// Introduced in 4c to stay within Bevy's 16-param limit. Extended in 4f
/// to also absorb `tag_cache` (reduces `process_action_system` to ≤ 14 params).
/// Renamed `VisualAssets` per D6; previously `RenderResources`.
#[derive(SystemParam)]
pub struct VisualAssets<'w, 's> {
    pub grid_offset: Res<'w, HexGridOffset>,
    pub tokens: Query<'w, 's, (Entity, &'static UnitToken)>,
    pub mats: Res<'w, HexMaterials>,
    pub token_mesh: Res<'w, TokenMesh>,
    pub tag_cache: Res<'w, AbilityTagCache>,
}

/// Bundles the ECS queries that `build_ecs_content_view` needs to build the
/// engine content adapter.  Decouples content-data reads from visual resources
/// in system signatures.
///
/// Used by `process_action_system` and `bootstrap_combat_state`.
#[derive(SystemParam)]
pub struct ContentParams<'w, 's> {
    pub aura_q: Query<'w, 's, (Entity, &'static AuraSource), Without<Dead>>,
    pub phases_q: Query<'w, 's, (Entity, &'static EnemyPhases)>,
}

// ── Queue Resources for deferred ECS side-effects ────────────────────────────

/// Deferred list of units to mark `Dead` this `Execute` frame.
///
/// Producers: `translate_tick_events`, `translate_cast_events`,
/// `translate_move_events`. Consumer: `apply_pending_deaths_system`.
#[derive(Resource, Default)]
pub struct PendingDeathInserts(pub Vec<UnitId>);

/// Deferred turn-lifecycle mutations to apply after `project_state_to_ecs`.
///
/// Producers: `translate_end_turn_events` (and callers that delegate to it).
/// Consumer: `apply_pending_turn_lifecycle_system`.
#[derive(Resource, Default)]
pub struct PendingTurnLifecycle {
    pub remove_active: Vec<UnitId>,
    pub insert_active: Vec<UnitId>,
    /// When true, a `RoundStarted` was seen this frame; `StartRound` transition
    /// is scheduled and `insert_active` is suppressed because `build_turn_order`
    /// will set `ActiveCombatant` on the new actor during re-entry.
    pub round_started: bool,
}

/// Deferred animation pushes to apply after `project_state_to_ecs`.
///
/// Producer: `translate_move_events`. Consumer: `apply_pending_animations_system`.
#[derive(Resource, Default)]
pub struct PendingAnimations(pub Vec<PendingAnim>);

// ── apply-systems for the new queue Resources ─────────────────────────────────

/// Drains `PendingDeathInserts` and inserts the `Dead` marker component.
///
/// Runs after `process_action_system`, before `project_state_to_ecs`.
pub fn apply_pending_deaths_system(
    mut pending: ResMut<PendingDeathInserts>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
) {
    for uid in std::mem::take(&mut pending.0) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).insert(Dead);
        }
    }
}

/// Drains `PendingTurnLifecycle`: removes/inserts `ActiveCombatant` and
/// schedules `CombatPhase::StartRound` when a new round began.
///
/// Runs after `apply_pending_deaths_system`, before `project_state_to_ecs`.
pub fn apply_pending_turn_lifecycle_system(
    mut pending: ResMut<PendingTurnLifecycle>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut next_phase: Option<ResMut<NextState<CombatPhase>>>,
) {
    for uid in std::mem::take(&mut pending.remove_active) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).remove::<ActiveCombatant>();
        }
    }
    if pending.round_started {
        if let Some(ref mut np) = next_phase {
            np.set(CombatPhase::StartRound);
        }
        // round_started suppresses insert_active: build_turn_order will set
        // ActiveCombatant on the new actor during the next StartRound chain.
        pending.insert_active.clear();
        pending.round_started = false;
    } else {
        for uid in std::mem::take(&mut pending.insert_active) {
            if let Some(ent) = id_map.get_entity(uid) {
                commands.entity(ent).insert(ActiveCombatant);
            }
        }
    }
}

/// Drains `PendingAnimations` into `AnimationQueue`.
///
/// Runs after `project_state_to_ecs`, before `apply_phase_transitions_system`.
pub fn apply_pending_animations_system(
    mut pending: ResMut<PendingAnimations>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    for anim in std::mem::take(&mut pending.0) {
        anim_queue.0.push_back(anim);
    }
}
// ── spawn_ecs_entity_from_engine_unit ────────────────────────────────────────

/// Instantiate a new ECS combatant entity from a unit already present in the
/// engine state.  Called from `translate_cast_events` when `Event::UnitSpawned`
/// arrives; replaces the old `apply_spawn_system` + `SpawnUnit` message path.
///
/// Returns the new `Entity`, or `None` if the template is not in content
/// (should not happen — engine already validated the template before emitting
/// the event, but guards are cheap).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_ecs_entity_from_engine_unit(
    uid: combat_engine::state::UnitId,
    summoner_entity: Entity,
    pos: hexx::Hex,
    template_id: &str,
    team: combat_engine::state::Team,
    commands: &mut Commands,
    id_map: &mut UnitIdMap,
    positions: &mut HexPositions,
    active_content: &crate::content::content_view::ActiveContent,
    tag_cache: &AbilityTagCache,
    mats: &HexMaterials,
    token_mesh: &TokenMesh,
    grid_offset: &HexGridOffset,
    log: &mut CombatLog,
) -> Option<Entity> {
    use crate::game::components::Team as EcsTeam;

    let template = active_content.unit_templates.get(template_id)?;
    let equipment = Equipment {
        main_hand: Some(template.equipment.main_hand.clone()),
        off_hand: template.equipment.off_hand.clone(),
        chest: template.equipment.chest.clone(),
        legs: template.equipment.legs.clone(),
        feet: template.equipment.feet.clone(),
    };
    let effective = active_content.effective_stats(&template.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);
    let race_name = active_content.races.get(&template.race).map_or("", |r| r.name.as_str());
    let display_name = if race_name.is_empty() {
        template.name.clone()
    } else {
        format!("{} {}", race_name, template.name)
    };
    let ecs_team = match team {
        combat_engine::state::Team::Player => EcsTeam::Player,
        combat_engine::state::Team::Enemy => EcsTeam::Enemy,
    };
    let role = infer_profile(&template.ability_ids, effective.max_hp, armor, active_content, tag_cache);

    let mut ec = commands.spawn((
        Name::new(display_name.clone()),
        enemy_bundle(effective, armor, template.speed, template.ability_ids.clone(), equipment),
        role,
        AiMemory::default(),
        SummonedBy(summoner_entity),
    ));
    // enemy_bundle forces Team::Enemy — overwrite with actual team.
    ec.insert(Faction(ecs_team));
    if template.resources.rage_max > 0 {
        ec.insert(Rage::new(template.resources.rage_max));
    }
    if template.resources.mana_max > 0 {
        ec.insert(Mana::new(template.resources.mana_max));
    }
    if template.resources.energy_max > 0 {
        ec.insert(Energy::new(template.resources.energy_max));
    }
    if let Some(ref p) = template.path {
        ec.insert(CombatPath(p.clone()));
    }
    let new_entity = ec.id();

    positions.insert(new_entity, pos);
    id_map.insert(new_entity, uid);

    let pixel = LAYOUT.hex_to_world_pos(pos) + grid_offset.0;
    let token_material = match ecs_team {
        EcsTeam::Player => mats.token_player.clone(),
        EcsTeam::Enemy => mats.token_enemy.clone(),
    };
    commands.spawn((
        UnitToken(new_entity),
        Mesh2d(token_mesh.token.clone()),
        MeshMaterial2d(token_material),
        Transform::from_xyz(pixel.x, pixel.y, 0.15),
    ));

    log.push(CombatEvent::Summoned {
        summoner: summoner_entity,
        summon_name: display_name,
    });

    Some(new_entity)
}

// ── translate_events: unified bridge translator ───────────────────────────────

/// Cast-flow context — marker that the current translate_events call is
/// processing an `Action::Cast` event stream.
///
/// Cast-specific events (`UnitHealed`, `StatusApplied`, `CritFailed`,
/// `SpawnBlocked`) use `ctx.cast.is_some()` as a discriminant.
/// `Event::UnitSpawned` is handled in a separate post-pass at the callsite
/// because it requires `&mut Commands` which cannot be stored in `TranslateCtx`
/// without propagating Bevy's system-scoped `Commands` lifetime.
struct CastCtx {
    // Marker struct — no fields needed; cast-specific behavior is gated on
    // ctx.cast.is_some() inside translate_one.
    _phantom: (),
}

/// Move-flow context — fields only needed when translating `Action::Move` events.
struct MoveCtx<'a> {
    actor: Entity,
    combat_state: &'a CombatStateRes,
    grid_offset: &'a HexGridOffset,
    /// Aggregated start position for the single `UnitMoved` log entry.
    first_from: Option<hexx::Hex>,
    /// Aggregated end position for the single `UnitMoved` log entry.
    last_to: Option<hexx::Hex>,
    /// All waypoints (world-space) for the movement animation.
    waypoints: Vec<Vec2>,
    /// State machine for AoO pairing: `ReactionFired` immediately precedes the
    /// paired `UnitDamaged` in the event stream (decision 6.3).
    /// PRESERVE: do not fuse into `Event::AooDamaged` here — deferred to a
    /// future S-task (the second fusion candidate after S5's DotDamaged).
    pending_aoo_target: Option<UnitId>,
}

/// Bundle of all mutable state shared across `translate_events`.
///
/// The four formerly-separate translator functions each closed over a different
/// subset of this state; now one exhaustive `match` in `translate_one` branches
/// on `ctx.cast` / `ctx.move_` presence to recover the same context-dependent
/// behaviour.
///
/// Lifetime `'a` is the lifetime of the Bevy system parameter borrows passed in
/// from `process_action_system` / `bootstrap_combat_state`.
struct TranslateCtx<'a> {
    /// Shared by every translator.  Held as `&mut` so the `UnitSpawned` arm
    /// can pass it to `spawn_ecs_entity_from_engine_unit` (which registers the
    /// new entity).  Read-only arms dereference via `&*ctx.id_map`.
    log: &'a mut CombatLog,
    id_map: &'a mut UnitIdMap,
    pending_deaths: &'a mut PendingDeathInserts,
    pending_lifecycle: &'a mut PendingTurnLifecycle,
    /// Cast-flow-specific state (None outside `Action::Cast` translation).
    cast: Option<CastCtx>,
    /// Move-flow-specific state (None outside `Action::Move` translation).
    move_: Option<MoveCtx<'a>>,
}

/// Unified bridge event translator — one exhaustive `match` over every
/// `Event` variant.
///
/// Replaces four formerly-separate translator functions:
/// - `translate_tick_events`     (dot-damage, death, rage, mana-regen)
/// - `translate_end_turn_events` (turn/round lifecycle, aura status changes)
/// - `translate_cast_events`     (ability log entry, heal, status, crit-fail, spawn)
/// - `translate_move_events`     (waypoint aggregation, AoO pairing, movement anim)
///
/// Context-dependent behaviour (cast vs move vs tick) is driven by the
/// presence of `ctx.cast` / `ctx.move_` sub-structs:
///
/// - `UnitDamaged` in tick context: pierce-aware `armor_reduced` formula.
///   In cast context: passes `mitigation` as-is (engine zeroes it for piercing
///   casts). In move context: only handled when paired with a preceding
///   `ReactionFired` (AoO state machine).
/// - `UnitMoved`, `ReactionFired`: only meaningful in move context.
/// - `CritFailed`, `UnitSpawned`, `SpawnBlocked`, `UnitHealed`, `StatusApplied`:
///   only meaningful in cast context.
/// - Turn/round/aura events: always meaningful (B5 can emit them in any flow).
///
/// After the loop, callers in move context must call `finalize_move` to emit
/// the aggregated `UnitMoved` log entry and enqueue the movement animation.
fn translate_events(events: &[Event], ctx: &mut TranslateCtx<'_>) {
    for ev in events {
        translate_one(ev, ctx);
    }
}

#[allow(clippy::too_many_lines)]
fn translate_one(ev: &Event, ctx: &mut TranslateCtx<'_>) {
    match ev {
        // ── Move-specific: position tracking ─────────────────────────────────
        Event::UnitMoved { from, to, .. } => {
            // no-op: not produced during Cast or tick actions
            if let Some(mv) = ctx.move_.as_mut() {
                if mv.first_from.is_none() {
                    mv.first_from = Some(*from);
                    mv.waypoints.push(LAYOUT.hex_to_world_pos(*from) + mv.grid_offset.0);
                }
                mv.last_to = Some(*to);
                mv.waypoints.push(LAYOUT.hex_to_world_pos(*to) + mv.grid_offset.0);
            }
        }

        // ── Move-specific: AoO state machine ─────────────────────────────────
        Event::ReactionFired { kind, against, .. } => {
            // AoO reactions set the pending target for the next UnitDamaged pair.
            // Non-AoO reactions have no bridge representation yet.
            if matches!(kind, ReactionKind::OpportunityAttack) {
                if let Some(mv) = ctx.move_.as_mut() {
                    mv.pending_aoo_target = Some(*against);
                }
            }
        }

        // ── UnitDamaged: three context-dependent behaviours ───────────────────
        //
        // Move:  only AoO-paired damage (decision 6.3 — pending_aoo_target machine).
        // Cast:  pass mitigation as-is (engine zeroes it for piercing casts).
        // Tick:  pierce-aware formula (DoT always pierces armor — use pierces flag).
        Event::UnitDamaged { target, amount, raw, mitigation, pierces, source } => {
            if let Some(mv) = ctx.move_.as_mut() {
                if mv.pending_aoo_target == Some(*target) {
                    let Some(attacker_ent) = ctx.id_map.get_entity(*source) else {
                        mv.pending_aoo_target = None;
                        return;
                    };
                    let Some(target_ent) = ctx.id_map.get_entity(*target) else {
                        mv.pending_aoo_target = None;
                        return;
                    };
                    let killed = mv
                        .combat_state
                        .0
                        .unit(*target)
                        .map(|u| !u.is_alive())
                        .unwrap_or(false);
                    ctx.log.push(CombatEvent::OpportunityAttack {
                        attacker: attacker_ent,
                        target: target_ent,
                        damage: *amount,
                        killed,
                    });
                    mv.pending_aoo_target = None;
                }
                // Non-AoO damage on Move is not possible — silently ignore.
            } else if ctx.cast.is_some() {
                // Cast context: engine already zeroes mitigation for piercing casts.
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else { return };
                ctx.log.push(CombatEvent::DamageResult {
                    target: tgt_ent,
                    raw: raw.round() as i32,
                    armor_reduced: *mitigation,
                    final_damage: *amount,
                });
            } else {
                // Tick context: apply pierce-aware formula.
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    let armor_reduced = if *pierces { 0 } else { *mitigation };
                    ctx.log.push(CombatEvent::DamageResult {
                        target: tgt_ent,
                        raw: raw.round() as i32,
                        armor_reduced,
                        final_damage: *amount,
                    });
                }
            }
        }

        // ── DoT damage (fused atomic, tick context only) ──────────────────────
        Event::DotDamaged { target, source, source_status, raw, mitigation, pierces, amount } => {
            // no-op: DotDamaged not produced during Cast or Move actions
            if ctx.cast.is_none() && ctx.move_.is_none() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else { return };
                let Some(src_ent) = ctx.id_map.get_entity(*source) else { return };
                ctx.log.push(CombatEvent::DotDamaged {
                    target: tgt_ent,
                    source: src_ent,
                    source_status: source_status.clone(),
                    raw: *raw,
                    mitigation: *mitigation,
                    pierces: *pierces,
                    amount: *amount,
                });
            }
        }

        // ── Zero-damage status tick ───────────────────────────────────────────
        Event::StatusTicked { .. } => {
            // no-op: zero-damage ticks have no CombatLog entry in any context
        }

        // ── Status changes ────────────────────────────────────────────────────
        Event::StatusRemoved { target, status } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusExpired {
                    target: tgt_ent,
                    status: status.clone(),
                });
            }
        }
        Event::StatusApplied { target, status } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    ctx.log.push(CombatEvent::StatusApplied {
                        target: tgt_ent,
                        status: status.clone(),
                    });
                }
            }
        }

        // ── Aura events (turn/round-boundary, any context) ────────────────────
        Event::AuraStatusGained { target, status_id, .. } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusApplied {
                    target: tgt_ent,
                    status: status_id.clone(),
                });
            }
        }
        Event::AuraStatusLost { target, status_id, .. } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusExpired {
                    target: tgt_ent,
                    status: status_id.clone(),
                });
            }
        }

        // ── Death ─────────────────────────────────────────────────────────────
        Event::UnitDied { unit } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::UnitDied { entity: ent });
                ctx.pending_deaths.0.push(*unit);
            }
        }

        // ── Healing (cast only) ───────────────────────────────────────────────
        Event::UnitHealed { target, amount } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else { return };
                ctx.log.push(CombatEvent::HealResult {
                    target: tgt_ent,
                    amount: *amount,
                });
            }
        }

        // ── Resource changes ──────────────────────────────────────────────────
        Event::RageGained { unit, current, max } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::RageGained {
                    actor: ent,
                    current: *current,
                    max: *max,
                });
            }
        }
        Event::ManaRegenerated { unit, current, max } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::ManaChanged {
                    actor: ent,
                    current: *current,
                    max: *max,
                });
            }
        }
        Event::EnergyRegenerated { .. } => {
            // no-op: energy regen has no CombatLog entry yet
        }

        // ── Crit-fail (cast only) ─────────────────────────────────────────────
        Event::CritFailed { actor: actor_uid, outcome } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(actor_ent) = ctx.id_map.get_entity(*actor_uid) else { return };
                match outcome {
                    combat_engine::CritFailOutcome::Miss => {
                        ctx.log.push(CombatEvent::CriticalMiss { actor: actor_ent });
                    }
                    _ => {
                        ctx.log.push(CombatEvent::CritFailSideEffect {
                            actor: actor_ent,
                            outcome: CritFailOutcomeEcs::from(outcome),
                        });
                    }
                }
            }
        }

        // ── Spawn / despawn (cast only) ───────────────────────────────────────
        Event::UnitSpawned { .. } => {
            // no-op in translate_one: UnitSpawned requires &mut Commands which
            // cannot be stored in TranslateCtx without propagating Bevy's system-
            // scoped Commands lifetime through the borrow graph.  Instead, callers
            // in cast context handle UnitSpawned in a separate post-pass after
            // translate_events returns (same pattern as PhaseEntered).
        }
        Event::SpawnBlocked { summoner: summoner_uid, reason, .. } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(summoner_entity) = ctx.id_map.get_entity(*summoner_uid) else { return };
                ctx.log.push(CombatEvent::SummonBlocked {
                    summoner: summoner_entity,
                    reason: SpawnBlockedReasonEcs::from(reason),
                });
            }
        }

        // ── Turn / round lifecycle (any context after B5) ─────────────────────
        Event::TurnEnded { actor, cause } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.pending_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnEnded {
                    actor: ent,
                    cause: crate::game::combat_log::TurnEndCauseEcs::from(cause),
                });
            }
        }
        Event::TurnSkipped { actor, reason } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.pending_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnSkipped {
                    actor: ent,
                    reason: TurnSkipReasonEcs::from(reason),
                });
            }
        }
        Event::RoundStarted { round } => {
            ctx.log.push(CombatEvent::RoundStarted { round: *round });
            ctx.pending_lifecycle.round_started = true;
        }
        Event::TurnStarted { actor } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                if !ctx.pending_lifecycle.round_started {
                    // Mid-round handoff: insert ActiveCombatant on the new actor.
                    // After RoundStarted, build_turn_order inserts it on re-entry.
                    ctx.pending_lifecycle.insert_active.push(*actor);
                }
                ctx.log.push(CombatEvent::TurnStarted { actor: ent });
            }
        }

        // ── Action bookkeeping ────────────────────────────────────────────────
        Event::ActionStarted { .. } => {
            // no-op: action bookkeeping events have no CombatLog entry
        }
        Event::ActionFinished { .. } => {
            // no-op: action bookkeeping events have no CombatLog entry
        }

        // ── Phase transitions (handled at caller level) ───────────────────────
        Event::PhaseEntered { .. } => {
            // no-op: ECS writes for phase transitions are handled at the callsite
            // via pending_phases.0.push(...) after the translate_events call
        }
    }
}


/// Emit the `CombatEvent::AbilityUsed` preamble for a cast action.
/// Called once before `translate_events` in the cast flow.
fn emit_ability_used(
    actor: Entity,
    ability: &combat_engine::AbilityId,
    target: Entity,
    target_pos: hexx::Hex,
    active_content: &ActiveContent,
    log: &mut CombatLog,
) {
    let (ability_name, is_aoe, cost_str) = active_content
        .abilities
        .get(ability)
        .map(|def| {
            let is_aoe = !matches!(def.aoe, crate::content::abilities::AoEShape::None);
            (def.name.clone(), is_aoe, format!("AP={}", def.cost_ap))
        })
        .unwrap_or_else(|| (ability.0.clone(), false, String::new()));

    log.push(CombatEvent::AbilityUsed {
        actor,
        ability_name,
        target,
        target_pos,
        is_aoe,
        cost_str,
    });
}

// ── process_action_system ──────────────────────────────────────────────────────

/// `Update` system — authoritative action handler via `combat_engine::step()`.
///
/// Reads `ActionInput` messages, calls `step()` against the mirrored
/// `CombatStateRes`, and translates the resulting `Event` stream into Bevy-land
/// side effects (CombatLog entries, Dead markers, movement animations).
/// The engine is the sole owner of both `Action::Move` (since Phase 1) and
/// `Action::Cast` (since Phase 2 step 9d).
///
/// Wired with a real ECS-backed `EcsContentView` so the engine can fire AoO
/// reactions correctly.  `project_state_to_ecs` (chained immediately after)
/// writes the engine mutations back to ECS components.  The engine is now the
/// sole writer for hp / rage / mana / statuses — the clobber bug documented in
/// earlier TODO comments is resolved by the deletion of `apply_effects_system`
/// in step 9d.
///
/// Runs in `CombatStep::Execute`, gated by `CombatPhase::AwaitCommand`.
pub fn process_action_system(
    mut commands: Commands,
    mut reader: MessageReader<ActionInput>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    active_content: Res<ActiveContent>,
    mut rng: ResMut<crate::combat::DiceRngRes>,
    mut log: ResMut<CombatLog>,
    mut positions: ResMut<HexPositions>,
    visuals: VisualAssets,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
    mut pending_deaths: ResMut<PendingDeathInserts>,
    mut pending_lifecycle: ResMut<PendingTurnLifecycle>,
    mut pending_animations: ResMut<PendingAnimations>,
    mut trace_writer: ResMut<crate::combat::ai::log::engine_trace::EngineTraceWriter>,
) {
    for msg in reader.read() {
        match msg {
            ActionInput::Move { actor, path } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for entity {:?} — skipping",
                        actor
                    );
                    continue;
                };

                let action = Action::Move {
                    actor: actor_uid,
                    path: path.clone(),
                };

                let content = build_ecs_content_view(&active_content);

                let action_for_trace = action.clone();
                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection so a crash mid-projection
                        // doesn't corrupt the trace (plan spec §4 wiring note).
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) = trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash) {
                            warn!("Engine trace step write failed: {e}");
                        }
                        let move_ctx = MoveCtx {
                            actor: *actor,
                            combat_state: &combat_state,
                            grid_offset: &visuals.grid_offset,
                            first_from: None,
                            last_to: None,
                            waypoints: Vec::new(),
                            pending_aoo_target: None,
                        };
                        // Scoped block so ctx's borrow of `log` ends before finalize_move.
                        let (final_from, final_to, final_waypoints, final_actor) = {
                            let mut ctx = TranslateCtx {
                                log: &mut log,
                                id_map: &mut id_map,
                                pending_deaths: &mut pending_deaths,
                                pending_lifecycle: &mut pending_lifecycle,
                                cast: None,
                                move_: Some(move_ctx),
                            };
                            translate_events(&events, &mut ctx);
                            let mv = ctx.move_.take().unwrap();
                            (mv.first_from, mv.last_to, mv.waypoints, mv.actor)
                        };
                        // Emit aggregated UnitMoved and enqueue animation (ctx dropped above).
                        if let (Some(from), Some(to)) = (final_from, final_to) {
                            log.push(CombatEvent::UnitMoved { actor: final_actor, from, to });
                        }
                        if !final_waypoints.is_empty() {
                            if let Some((token_entity, _)) = visuals.tokens.iter().find(|(_, t)| t.0 == final_actor) {
                                pending_animations.0.push(PendingAnim::Movement {
                                    token: token_entity,
                                    waypoints: final_waypoints,
                                });
                            }
                        }
                        // AoO on a move can cross a phase threshold; queue for apply system.
                        for ev in &events {
                            if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                                pending_phases.0.push((*unit, *phase_idx));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                    }
                }
            }
            ActionInput::Cast { actor, ability, target, target_pos } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for cast actor {:?} — skipping",
                        actor
                    );
                    continue;
                };
                let Some(target_uid) = id_map.get_id(*target) else {
                    warn!(
                        "process_action_system: no UnitId for cast target {:?} — skipping",
                        target
                    );
                    continue;
                };

                let action = Action::Cast {
                    actor: actor_uid,
                    ability: ability.clone(),
                    target: target_uid,
                    target_pos: *target_pos,
                };

                let content = build_ecs_content_view(&active_content);

                let action_for_trace = action.clone();
                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) = trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash) {
                            warn!("Engine trace step write failed: {e}");
                        }
                        emit_ability_used(*actor, ability, *target, *target_pos, &active_content, &mut log);
                        {
                            let cast_ctx = CastCtx { _phantom: () };
                            let mut ctx = TranslateCtx {
                                log: &mut log,
                                id_map: &mut id_map,
                                pending_deaths: &mut pending_deaths,
                                pending_lifecycle: &mut pending_lifecycle,
                                cast: Some(cast_ctx),
                                move_: None,
                            };
                            translate_events(&events, &mut ctx);
                        } // ctx drops here, releasing &mut id_map
                        // Post-pass: handle UnitSpawned separately (needs &mut Commands
                        // which cannot be stored in TranslateCtx — same pattern as PhaseEntered).
                        for ev in &events {
                            if let Event::UnitSpawned { uid, summoner: summoner_uid, pos, template_id, team } = ev {
                                let Some(summoner_entity) = id_map.get_entity(*summoner_uid) else { continue };
                                spawn_ecs_entity_from_engine_unit(
                                    *uid,
                                    summoner_entity,
                                    *pos,
                                    template_id,
                                    *team,
                                    &mut commands,
                                    &mut id_map,
                                    &mut positions,
                                    &active_content,
                                    &visuals.tag_cache,
                                    &visuals.mats,
                                    &visuals.token_mesh,
                                    &visuals.grid_offset,
                                    &mut log,
                                );
                            }
                        }
                        // Queue phase transitions from cast events (most common case:
                        // boss crosses HP threshold from a direct damage spell).
                        for ev in &events {
                            if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                                pending_phases.0.push((*unit, *phase_idx));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: Cast step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                        // Cast failed validation — engine state is rolled back, so
                        // don't end the turn; let the user retry or end manually.
                    }
                }
            }
            ActionInput::EndTurn { actor } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for EndTurn actor {:?} — skipping",
                        actor
                    );
                    continue;
                };

                let content = build_ecs_content_view(&active_content);

                let end_action = Action::EndTurn { actor: actor_uid };
                match step(&mut combat_state.0, end_action.clone(), &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) = trace_writer.write_step(&end_action, &events, ctx.rng_calls, hash) {
                            warn!("Engine trace step write failed: {e}");
                        }
                        let mut ctx = TranslateCtx {
                            log: &mut log,
                            id_map: &mut id_map,
                            pending_deaths: &mut pending_deaths,
                            pending_lifecycle: &mut pending_lifecycle,
                            cast: None,
                            move_: None,
                        };
                        translate_events(&events, &mut ctx);
                        // DoT ticks at end of turn can cross a phase threshold.
                        for ev in &events {
                            if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                                pending_phases.0.push((*unit, *phase_idx));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: EndTurn step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                    }
                }
            }
        }
    }
}

// ── project_state_to_ecs system ──────────────────────────────────────────────

/// Query alias for the ECS components the projector writes.
type ProjectionRow<'a> = (
    &'a mut Vital,
    &'a mut ActionPoints,
    &'a mut Reactions,
    Has<BonusMovement>,
    Option<&'a mut Rage>,
    Option<&'a mut Mana>,
    Option<&'a mut Energy>,
    Option<&'a mut StatusEffects>,
);

/// `Update` system — writes engine `CombatState` back to ECS components.
///
/// Projects:
/// - `pos`              → `HexPositions` (alive) / `HexCorpses` (dead)
/// - `hp`               → `Vital.hp`
/// - `movement_points`  → `ActionPoints.movement_points`
/// - `reactions_left`   → `Reactions.remaining`
/// - `reactions_max`    → `Reactions.max`
///
/// Initialise engine `CombatState` from the current ECS snapshot.
///
/// Called on `OnEnter(CombatPhase::AwaitCommand)` once per round (after
/// `build_turn_order` refills AP + reactions into ECS) so the engine has
/// a fresh, authoritative copy of all unit state.
///
/// **5c.1 addition:** also populates the three new per-combat `Unit` fields:
/// - `caster_context` — from `CombatStats` + `Equipment` + optional `CombatPath`
/// - `auras`          — from `AuraSource` ECS component (alive sources only)
/// - `enemy_phases`   — from `EnemyPhases.pending` ECS component
///
/// MP+reactions refill happens in `StartRound` (symmetric with `start_actor_turn`).
///
/// Engine is authoritative for state; ECS is a read-only projection.
///
/// **Layer model:** alive units live in [`HexPositions`] (one-per-hex invariant);
/// dead units live in [`HexCorpses`] (multi-occupant). The two branches below
/// are order-insensitive: whichever entity is iterated first, `remove` on the
/// wrong layer is a no-op, so there is no cross-contamination.
pub fn project_state_to_ecs(
    mut commands: Commands,
    combat_state: Res<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    mut positions: ResMut<HexPositions>,
    mut corpses: ResMut<HexCorpses>,
    mut combatants: Query<ProjectionRow, With<Combatant>>,
) {
    for unit in combat_state.0.units() {
        let Some(entity) = id_map.get_entity(unit.id) else {
            // Unit not yet mapped to ECS — skip silently.
            continue;
        };

        if unit.hp <= 0 {
            // Transition to corpse layer (idempotent — engine.unit.pos is stable).
            positions.remove(&entity);
            corpses.insert(entity, unit.pos);
            // Still sync hp=0 so Vital reflects death; skip AP/MP/Rage/Mana/Energy/Status.
            if let Ok((mut vital, _, _, _, _, _, _, _)) = combatants.get_mut(entity) {
                vital.hp = unit.hp;
            }
            continue;
        }

        // Alive — occupancy layer.
        positions.insert(entity, unit.pos);

        // Write Vital / ActionPoints / Reactions / Rage / Mana / Energy / StatusEffects.
        if let Ok((mut vital, mut ap, mut reactions, has_bonus, rage_opt, mana_opt, energy_opt, status_effects_opt)) =
            combatants.get_mut(entity)
        {
            vital.hp = unit.hp;
            ap.action_points = unit.action_points;
            ap.movement_points = unit.movement_points;
            reactions.remaining = unit.reactions_left as u8;
            reactions.max       = unit.reactions_max as u8;

            if has_bonus && ap.movement_points == 0 {
                commands.entity(entity).remove::<BonusMovement>();
            }

            // Project rage.current when both sides carry a rage pool.
            if let (Some((engine_current, _engine_max)), Some(mut ecs_rage)) =
                (unit.rage, rage_opt)
            {
                ecs_rage.current = engine_current;
            }

            // Project mana.current when both sides carry a mana pool.
            if let (Some((current, _max)), Some(mut mana_comp)) = (unit.mana, mana_opt) {
                mana_comp.current = current;
            }

            // Project energy.current when both sides carry an energy pool.
            if let (Some((current, _max)), Some(mut energy_comp)) = (unit.energy, energy_opt) {
                energy_comp.current = current;
            }

            // Merge statuses: preserve ECS entries the engine doesn't know about.
            if let Some(mut status_effects) = status_effects_opt {
                let engine_known: std::collections::HashSet<(&combat_engine::StatusId, UnitId)> =
                    unit.statuses.iter().map(|s| (&s.id, s.applier)).collect();

                let preserved: Vec<crate::game::components::ActiveStatus> = status_effects
                    .0
                    .iter()
                    .filter(|ecs_s| {
                        !engine_known.contains(&(&ecs_s.id, entity_to_uid(ecs_s.applier)))
                    })
                    .cloned()
                    .collect();

                let mut new_list: Vec<crate::game::components::ActiveStatus> = preserved;
                for engine_s in &unit.statuses {
                    let applier_entity = id_map.get_entity(engine_s.applier).unwrap_or(entity);
                    new_list.push(crate::game::components::ActiveStatus {
                        id: engine_s.id.clone(),
                        rounds_remaining: engine_s.rounds_remaining,
                        dot_per_tick: engine_s.dot_per_tick,
                        applier: applier_entity,
                    });
                }

                status_effects.0 = new_list;
            }
        }
    }
}

// ── bootstrap_combat_state system ────────────────────────────────────────────

/// Bootstrap engine `CombatState` from the current ECS snapshot.
///
/// Runs at the tail of the `CombatPhase::StartRound` chain, after
/// `build_turn_order` has populated `Res<TurnQueue>` and incremented
/// `CombatContext.round`.
///
/// Responsibilities (formerly split across `init_state_from_ecs` and
/// `engine_start_first_turn_system`):
/// - Build `CombatState` from ECS via `from_ecs` (includes V2 status-aggregate recompute).
/// - Populate per-unit `caster_context`, `aoo_dice`, `auras`, `enemy_phases`.
/// - Set engine turn queue from `Res<TurnQueue>`.
/// - Prime the first actor's turn (AP/MP refill, regen, status tick).
///
/// Idempotent: returns immediately if `combat_state.0` already has units
/// (round 2+ re-entries, second-combat-in-session teardown races).
pub fn bootstrap_combat_state(
    combatants: Query<CombatantRow, With<Combatant>>,
    positions: Res<HexPositions>,
    corpses: Res<HexCorpses>,
    combat_context: Res<CombatContext>,
    ecs_queue: Res<TurnQueue>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    caster_q: Query<(Entity, &Equipment, &CombatStats, Option<&CombatPath>), With<Combatant>>,
    aoo_q: Query<(Entity, &Equipment, &CombatStats, &Abilities, Has<Dead>), With<Combatant>>,
    aura_q: Query<(Entity, &AuraSource), Without<Dead>>,
    phases_q: Query<(Entity, &EnemyPhases), With<Combatant>>,
    active_content: Res<ActiveContent>,
    mut log: ResMut<CombatLog>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
    mut pending_deaths: ResMut<PendingDeathInserts>,
    mut pending_lifecycle: ResMut<PendingTurnLifecycle>,
) {
    // Idempotency guard: engine state evolves authoritatively via step() on
    // round 2+; re-importing would discard those mutations.
    if !combat_state.0.units().is_empty() {
        return;
    }

    use crate::content::encounters::AuraAffects;

    let mut state = from_ecs(&combatants, &positions, &corpses, combat_context.round, &mut id_map, &active_content);

    // ── Populate per-unit combat fields ──────────────────────────────────────

    // caster_context: built from Equipment + CombatStats (alive and dead units).
    for (entity, equipment, stats, combat_path) in caster_q.iter() {
        let Some(uid) = id_map.get_id(entity) else { continue };
        let bevy_ctx = CasterContext::new(stats, Some(equipment), &active_content.weapons);
        let crit_fail_outcome = combat_path
            .and_then(|cp| active_content.paths.get(&cp.0))
            .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
        let engine_ctx = combat_engine::CasterContext {
            str_mod: bevy_ctx.str_mod,
            int_mod: bevy_ctx.int_mod,
            spell_power: bevy_ctx.spell_power,
            weapon_dice: bevy_ctx.weapon_dice,
            crit_fail_outcome: crate::content::to_engine::crit_fail_outcome(&crit_fail_outcome),
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.caster_context = engine_ctx;
        }
    }

    // aoo_dice: alive units with a melee WeaponAttack ability + weapon equipped.
    // Mirrors the pre-5c.1 build_ecs_content_view AoO eligibility filter so
    // ranged units (no melee ability) don't AoO even though they have a weapon.
    // Strength modifier is baked in so engine's unit_aoo_dice returns the final formula.
    for (entity, equipment, stats, abilities, is_dead) in aoo_q.iter() {
        if is_dead { continue; }
        let Some(uid) = id_map.get_id(entity) else { continue };
        let has_melee = abilities.0.iter().any(|aid| {
            active_content.abilities.get(aid).is_some_and(|def| {
                matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
            })
        });
        if !has_melee { continue; }
        let ctx = CasterContext::new(stats, Some(equipment), &active_content.weapons);
        let Some(core_dice) = ctx.weapon_dice else { continue };
        let engine_dice = EngineDiceExpr::new(
            core_dice.count,
            core_dice.sides,
            core_dice.bonus + modifier(stats.strength),
        );
        if let Some(unit) = state.unit_mut(uid) {
            unit.aoo_dice = Some(engine_dice);
        }
    }

    // auras: from AuraSource components (alive sources only).
    for (entity, aura_src) in aura_q.iter() {
        let Some(uid) = id_map.get_id(entity) else { continue };
        let applies_to = match aura_src.affects {
            AuraAffects::Enemies => TeamRelation::Enemies,
            AuraAffects::Allies  => TeamRelation::Allies,
            AuraAffects::All     => TeamRelation::All,
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.auras.push(AuraDef {
                radius: aura_src.radius,
                status_id: aura_src.status.clone(),
                applies_to,
            });
        }
    }

    // enemy_phases: from EnemyPhases.pending.
    for (entity, phases) in phases_q.iter() {
        let Some(uid) = id_map.get_id(entity) else { continue };
        if let Some(unit) = state.unit_mut(uid) {
            unit.enemy_phases = phases.pending.iter().map(|phase| {
                let crate::content::encounters::PhaseTrigger::HpBelowPct(pct) = phase.trigger;
                let new_max_hp = phase.stats.as_ref().map(|s| s.max_hp).unwrap_or(0);
                combat_engine::PhaseEntry { pct, new_max_hp, heal_to_full: phase.heal_to_full }
            }).collect();
        }
    }

    // ── Set engine turn queue from ECS ────────────────────────────────────────
    let uid_order: Vec<UnitId> = ecs_queue
        .order
        .iter()
        .filter_map(|e| id_map.get_id(*e))
        .collect();
    // In production StartRound chain, build_turn_order always runs first so
    // uid_order is non-empty.  Tests that call bootstrap directly without
    // build_turn_order may have an empty queue — skip set_turn_queue in that
    // case (tests set ActiveCombatant manually instead).
    if !uid_order.is_empty() {
        state.set_turn_queue(uid_order, ecs_queue.index);
    }

    // ── Prime the first actor's turn ──────────────────────────────────────────
    // On round 2+, start_actor_turn is called by the engine's EndTurn cascade.
    // On round 1, the cascade hasn't fired yet, so bootstrap primes it here.
    if let Some(first_actor) = state.turn_queue.current() {
        let content = build_ecs_content_view(&active_content);
        let events = state.start_actor_turn(first_actor, &content);

        let mut tick_ctx = TranslateCtx {
            log: &mut log,
            id_map: &mut id_map,
            pending_deaths: &mut pending_deaths,
            pending_lifecycle: &mut pending_lifecycle,
            cast: None,
            move_: None,
        };
        translate_events(&events, &mut tick_ctx);

        // Queue ECS-only phase deltas (same pattern as process_action_system).
        for ev in &events {
            if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                pending_phases.0.push((*unit, *phase_idx));
            }
        }
    }

    combat_state.0 = state;
}

// ── reset_engine_mirrors ──────────────────────────────────────────────────────

/// Clears the engine-side mirrors (`CombatStateRes`, `UnitIdMap`,
/// `PendingPhaseTransitions`) so a fresh combat starts from a clean slate.
///
/// Without this reset, the next combat's `StartRound` system
/// `project_state_to_ecs` would iterate stale unit data from the previous
/// combat and try to write its positions into the freshly-cleared
/// `HexPositions` resource, colliding with the newly-spawned combatants.
///
/// Plain helper — both reset systems below delegate here so the "what counts
/// as an engine mirror" knowledge lives in one place. Add a new mirror? Update
/// this function only.
fn reset_engine_mirrors(
    combat_state: &mut CombatStateRes,
    id_map: &mut UnitIdMap,
    pending_phases: &mut PendingPhaseTransitions,
    pending_deaths: &mut PendingDeathInserts,
    pending_lifecycle: &mut PendingTurnLifecycle,
    pending_animations: &mut PendingAnimations,
) {
    *combat_state = CombatStateRes::default();
    id_map.clear();
    pending_phases.0.clear();
    pending_deaths.0.clear();
    pending_lifecycle.remove_active.clear();
    pending_lifecycle.insert_active.clear();
    pending_lifecycle.round_started = false;
    pending_animations.0.clear();
}

/// `OnExit(AppState::Combat)` system — natural combat-end teardown.
pub fn reset_engine_mirrors_on_exit_combat(
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
    mut pending_deaths: ResMut<PendingDeathInserts>,
    mut pending_lifecycle: ResMut<PendingTurnLifecycle>,
    mut pending_animations: ResMut<PendingAnimations>,
) {
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut pending_phases, &mut pending_deaths, &mut pending_lifecycle, &mut pending_animations);
}

/// `Update` system listening to `RestartCombat` messages. The restart flow
/// keeps `AppState::Combat`, so `OnExit` doesn't fire — we need an explicit
/// reader. Bevy permits multiple independent readers of the same message
/// stream, so this coexists with `restart_combat_system` (each has its own
/// cursor).
pub fn reset_engine_mirrors_on_restart(
    mut reader: MessageReader<RestartCombat>,
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
    mut pending_deaths: ResMut<PendingDeathInserts>,
    mut pending_lifecycle: ResMut<PendingTurnLifecycle>,
    mut pending_animations: ResMut<PendingAnimations>,
) {
    if reader.read().next().is_none() {
        return;
    }
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut pending_phases, &mut pending_deaths, &mut pending_lifecycle, &mut pending_animations);
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;
    use combat_engine::content::ContentView as EngineContentView;
    use combat_engine::StatusId;

    /// Regression test for the playtest bug "провокация не даёт прирост брони":
    /// `EcsContentView::status_bonuses` used to be a stub returning
    /// `StatusBonuses::default()` (always 0). Effect::RefreshAggregates
    /// reads bonuses through this method, so any status with
    /// `armor_bonus > 0` (`defending`, etc.) was silently dropped while
    /// `forces_targeting` continued to work — the latter is read via
    /// `status_def` which was never stubbed.
    ///
    /// Asserts that for the real `defending` status loaded from
    /// `assets/data/statuses.toml` (armor_bonus = 4), the bridge content
    /// view now reports the correct bonus.
    #[test]
    fn ecs_content_view_status_bonuses_reads_real_armor_bonus() {
        let active = ActiveContent(ContentView::load_global_for_tests());
        let view = build_ecs_content_view(&active);

        let defending = view.status_bonuses(&StatusId::from("defending"));
        assert_eq!(
            defending.armor_bonus, 4,
            "defending must report armor_bonus=4 from statuses.toml, not the stub default",
        );

        // Sanity: a status without armor_bonus stays at 0 (no false positives).
        let taunted = view.status_bonuses(&StatusId::from("taunted"));
        assert_eq!(taunted.armor_bonus, 0);
        assert_eq!(taunted.speed_bonus, 0);

        // Sanity: unknown status id falls back to default.
        let unknown = view.status_bonuses(&StatusId::from("__nonexistent__"));
        assert_eq!(unknown.armor_bonus, 0);
        assert_eq!(unknown.speed_bonus, 0);
    }

    /// End-to-end sanity: after `Effect::ApplyStatus(defending)` runs through
    /// the same `EcsContentView` path that production uses, the target unit's
    /// `armor_bonus` aggregate reflects the status (was 0 under the stub).
    #[test]
    fn refresh_aggregates_via_ecs_content_view_picks_up_defending_armor() {
        use combat_engine::effect::{apply_effect, Effect};
        use combat_engine::state::{CombatState, RoundPhase, Team, Unit, UnitId};
        use hexx::Hex;

        let active = ActiveContent(ContentView::load_global_for_tests());
        let view = build_ecs_content_view(&active);

        let unit = Unit {
            id: UnitId(1),
            team: Team::Player,
            pos: Hex::ZERO,
            hp: 20,
            max_hp: 20,
            armor: 3,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            base_speed: 3,
            speed: 3,
            action_points: 1,
            max_ap: 1,
            movement_points: 3,
            reactions_left: 1,
            reactions_max: 1,
            statuses: Vec::new(),
            rage: None,
            mana: None,
            energy: None,
            summoner: None,
            caster_context: combat_engine::CasterContext::default(),
            aoo_dice: None,
            auras: Vec::new(),
            enemy_phases: Vec::new(),
        };
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);

        // Mirror the production path: ApplyStatus derives RefreshAggregates.
        let (derived, _) = apply_effect(
            &mut state,
            &Effect::ApplyStatus {
                target: UnitId(1),
                status: StatusId::from("defending"),
                rounds: 1,
                dot_per_tick: 0,
                applier: UnitId(1),
            },
            &view,
        );
        // Process derived RefreshAggregates.
        for d in derived {
            apply_effect(&mut state, &d, &view);
        }

        let u = state.unit(UnitId(1)).unwrap();
        assert_eq!(u.armor_bonus, 4, "defending must contribute +4 armor_bonus");
        // Damage mitigation = armor + armor_bonus = 3 + 4 = 7.
        assert_eq!(u.armor + u.armor_bonus, 7);
    }
}
