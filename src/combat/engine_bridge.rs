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
//! - `init_state_from_ecs` — `OnEnter(CombatPhase::AwaitCommand)` system that
//!   initializes `CombatStateRes` from ECS once per round.  Engine is
//!   authoritative; ECS is a read-only projection (Phase 1 target).
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
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AuraSource, BonusMovement, CombatPath, CombatStats,
    Combatant, Dead, Energy, Equipment, EnemyPhases, Faction, Mana, Rage, Reactions, Speed,
    StatusEffects, SummonedBy, UnitToken, Vital,
};
use crate::game::bundles::enemy_bundle;
use crate::game::hex::LAYOUT;
use crate::game::messages::ActionInput;
use crate::game::resources::{CombatContext, HexPositions, TurnQueue};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

use combat_engine::{
    action::Action,
    content::{AuraDef, ContentView as EngineContentView, StatusBonuses, TeamRelation},
    event::Event,
    reaction::ReactionKind,
    state::{ActiveStatus, CombatState, Pool, RoundPhase, Unit, UnitId},
    step::step,
    StatusId,
};
use combat_engine::dice::DiceExpr as EngineDiceExpr;
use crate::core::modifier;

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
    Has<Dead>,
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
/// - `HexPositions` resource — unit positions
///
/// Dead units (`Has<Dead>`) are kept as tombstones (hp=0), matching the
/// `BattleSnapshot` convention so downstream code can filter by `is_alive()`.
///
/// **Note on `armor_bonus` / `speed` derivation from statuses:** Phase 0
/// sets them to base values.  Wiring `ContentView` into the bridge to call
/// `RefreshAggregates` is deferred to Phase 1+ since `step(Action::Move)` does
/// not consult `speed` for validation (only `movement_points`).
/// The sim shim (step 8) builds `CombatState` from `BattleSnapshot`, which
/// already carries the correct aggregates from `build_snapshot`.
pub fn from_ecs(
    combatants: &Query<CombatantRow, With<Combatant>>,
    positions: &HexPositions,
    round: u32,
    id_map: &mut UnitIdMap,
) -> CombatState {
    id_map.clear();

    let units: Vec<Unit> = combatants
        .iter()
        .filter_map(|(entity, vital, speed, ap, reactions, faction, statuses, rage, mana, energy_opt, is_dead)| {
            let pos = positions.get(&entity)?;

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

            Some(Unit {
                id: uid,
                team,
                pos,
                hp,
                max_hp: vital.max_hp,
                armor: vital.armor,
                armor_bonus: 0,           // Phase 0: status bonuses deferred to step 8+
                damage_taken_bonus: 0,    // Phase 0: recomputed by RefreshAggregates after init
                base_speed: speed.0,
                speed: speed.0,           // Phase 0: status speed_bonus deferred to step 8+
                action_points: ap.action_points,
                max_ap: ap.max_ap,
                movement_points: ap.movement_points,
                reactions_left: reactions.remaining as i32,
                reactions_max: reactions.max as i32,
                statuses: statuses_vec,
                rage: rage_pool,
                mana: mana_pool,
                energy: energy_pool,
                summoner: None,
                // Per-combat fields populated after from_ecs by init_state_from_ecs (5c.1).
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
/// `from_ecs` / `init_state_from_ecs`.
pub struct EcsContentView<'a> {
    active_content: &'a ActiveContent,
}

impl<'a> EngineContentView for EcsContentView<'a> {
    fn status_bonuses(&self, _id: &combat_engine::StatusId) -> StatusBonuses {
        StatusBonuses::default()
    }

    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
        self.active_content.abilities.get(id).map(|a| &a.engine)
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> {
        self.active_content.statuses.get(id).map(|s| &s.engine)
    }

    fn unit_template(&self, id: &str) -> Option<combat_engine::UnitTemplate> {
        let tpl = self.active_content.unit_templates.get(id)?;
        let equipment = Equipment {
            main_hand: Some(tpl.equipment.main_hand.clone()),
            off_hand: tpl.equipment.off_hand.clone(),
            chest: tpl.equipment.chest.clone(),
            legs: tpl.equipment.legs.clone(),
            feet: tpl.equipment.feet.clone(),
        };
        let effective = self.active_content.effective_stats(&tpl.stats, &equipment);
        let armor = self.active_content.equipment_armor(&equipment);
        Some(combat_engine::UnitTemplate {
            max_hp: effective.max_hp,
            armor,
            base_speed: tpl.speed,
            max_ap: 1, // templates carry no max_ap; matches CombatantBundle hardcoded default
            mana_max: tpl.resources.mana_max,
            energy_max: tpl.resources.energy_max,
            rage_max: tpl.resources.rage_max,
        })
    }
}

/// Deferred queue of phase transitions to apply at the end of `Execute`.
///
/// `process_action_system` / `engine_turn_start_system` push `(UnitId, phase_idx)`
/// for each `Event::PhaseEntered` they see.
/// `apply_phase_transitions_system` drains the queue and writes ECS-only deltas
/// (Name, Abilities, AxisProfile, EnemyPhases.pending pop, Dead removal, max_hp).
/// Running as a separate system after `project_state_to_ecs` avoids a Bevy
/// query conflict between the phase-write query and the projector's `&mut Vital`.
#[derive(Resource, Default)]
pub struct PendingPhaseTransitions(pub Vec<(UnitId, usize)>);

/// Build `EcsContentView` from the current ECS state.
/// Build `EcsContentView` from the current ECS state.
///
/// After 5c.1, `EcsContentView` only wraps `ActiveContent` — all per-combat
/// state (caster contexts, auras, phase triggers) now lives on engine `Unit`
/// fields and is populated once at init by `from_ecs`.
///
/// Called from `engine_turn_start_system`, `process_action_system`, and
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
/// `process_action_system` / `engine_turn_start_system` record `(unit, phase_idx)`
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
/// Used by `process_action_system` and `engine_turn_start_system`.
#[derive(SystemParam)]
pub struct ContentParams<'w, 's> {
    pub aura_q: Query<'w, 's, (Entity, &'static AuraSource), Without<Dead>>,
    pub phases_q: Query<'w, 's, (Entity, &'static EnemyPhases)>,
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

// ── translate_tick_events ─────────────────────────────────────────────────────

/// Translate an engine tick-event stream into `CombatLog` entries and ECS side-
/// effects (inserting `Dead`).
///
/// Shared by `engine_turn_start_system` (live-actor turn start) and
/// `advance_turn_system` (dead-actor sirota-DoT skip loop).
pub(crate) fn translate_tick_events(
    events: &[Event],
    id_map: &UnitIdMap,
    commands: &mut Commands,
    log: &mut CombatLog,
) {
    let mut pending_status_tick: Option<(UnitId, StatusId)> = None;

    for ev in events {
        match ev {
            Event::StatusTicked { target, status, .. } => {
                pending_status_tick = Some((*target, status.clone()));
            }
            Event::UnitDamaged { target, amount, .. } => {
                if let Some((tick_target, tick_status)) = pending_status_tick.take() {
                    if tick_target == *target {
                        if let Some(tgt_ent) = id_map.get_entity(*target) {
                            log.push(CombatEvent::PoisonTick {
                                target: tgt_ent,
                                status: tick_status,
                                damage: *amount,
                            });
                        }
                    } else {
                        if let Some(tgt_ent) = id_map.get_entity(*target) {
                            log.push(CombatEvent::DamageResult {
                                target: tgt_ent,
                                raw: *amount,
                                armor_reduced: 0,
                                final_damage: *amount,
                            });
                        }
                    }
                } else if let Some(tgt_ent) = id_map.get_entity(*target) {
                    log.push(CombatEvent::DamageResult {
                        target: tgt_ent,
                        raw: *amount,
                        armor_reduced: 0,
                        final_damage: *amount,
                    });
                }
            }
            Event::StatusRemoved { target, status } => {
                pending_status_tick = None;
                if let Some(tgt_ent) = id_map.get_entity(*target) {
                    log.push(CombatEvent::StatusExpired {
                        target: tgt_ent,
                        status: status.clone(),
                    });
                }
            }
            Event::UnitDied { unit } => {
                pending_status_tick = None;
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::UnitDied { entity: ent });
                    commands.entity(ent).insert(Dead);
                }
            }
            Event::RageGained { unit, current, max } => {
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::RageGained {
                        actor: ent,
                        current: *current,
                        max: *max,
                    });
                }
            }
            Event::ManaRegenerated { unit, current, max } => {
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::ManaChanged { actor: ent, current: *current, max: *max });
                }
            }
            Event::EnergyRegenerated { unit, current, max } => {
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::EnergyChanged { actor: ent, current: *current, max: *max });
                }
            }
            Event::UnitHealed { .. }
            | Event::StatusApplied { .. }
            | Event::ReactionFired { .. }
            | Event::UnitMoved { .. }
            | Event::ActionStarted { .. }
            | Event::ActionFinished { .. }
            | Event::CritFailed { .. }
            | Event::UnitSpawned { .. }
            | Event::SpawnBlocked { .. }
            | Event::TurnEnded { .. }
            | Event::TurnStarted { .. }
            | Event::TurnSkipped { .. }
            | Event::RoundStarted { .. }
            | Event::AuraStatusGained { .. }
            | Event::AuraStatusLost { .. }
            | Event::PhaseEntered { .. } => {}
        }
    }
}

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
    mut anim_queue: ResMut<AnimationQueue>,
    mut positions: ResMut<HexPositions>,
    visuals: VisualAssets,
    mut next_phase: Option<ResMut<NextState<CombatPhase>>>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
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
                        translate_move_events(
                            *actor,
                            &events,
                            &id_map,
                            &combat_state,
                            &mut commands,
                            &mut log,
                            &mut anim_queue,
                            &visuals.grid_offset,
                            &visuals.tokens,
                        );
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

                let mana_before = combat_state
                    .0
                    .unit(actor_uid)
                    .and_then(|u| u.mana)
                    .map(|(c, _)| c);

                let action_for_trace = action.clone();
                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) = trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash) {
                            warn!("Engine trace step write failed: {e}");
                        }
                        translate_cast_events(
                            *actor,
                            ability,
                            *target,
                            *target_pos,
                            mana_before,
                            &events,
                            &mut id_map,
                            &combat_state,
                            &active_content,
                            &mut commands,
                            &mut log,
                            &mut positions,
                            &visuals,
                        );
                        // Queue phase transitions from cast events (most common case:
                        // boss crosses HP threshold from a direct damage spell).
                        for ev in &events {
                            if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                                pending_phases.0.push((*unit, *phase_idx));
                            }
                        }
                        // End turn only when both AP and MP are exhausted, and the
                        // ability isn't GrantMovement (which exists specifically to
                        // extend the move budget). Mirrors the legacy
                        // `resolve_action_system` semantic — leftover MP after a cast
                        // lets the actor spend remaining movement before ending.
                        let is_grant_movement = active_content
                            .abilities
                            .get(ability)
                            .is_some_and(|d| matches!(d.effect, EffectDef::GrantMovement { .. }));
                        if let Some(unit) = combat_state.0.unit(actor_uid) {
                            if !is_grant_movement
                                && unit.action_points <= 0
                                && unit.movement_points <= 0
                            {
                                // AP and MP exhausted after cast — auto-end turn via engine.
                                let auto_end = Action::EndTurn { actor: actor_uid };
                                let end_content = build_ecs_content_view(&active_content);
                                if let Ok((end_events, end_ctx)) = step(&mut combat_state.0, auto_end.clone(), &mut rng.0, &end_content) {
                                    // Trace the auto-end-turn step too.
                                    let end_hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                                    if let Err(e) = trace_writer.write_step(&auto_end, &end_events, end_ctx.rng_calls, end_hash) {
                                        warn!("Engine trace auto-end-turn step write failed: {e}");
                                    }
                                    translate_end_turn_events(&end_events, &id_map, &mut commands, &mut log, &mut next_phase);
                                    // Phase transitions during auto-end-turn (e.g. DoT ticks).
                                    for ev in &end_events {
                                        if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
                                            pending_phases.0.push((*unit, *phase_idx));
                                        }
                                    }
                                }

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
                        translate_end_turn_events(
                            &events,
                            &id_map,
                            &mut commands,
                            &mut log,
                            &mut next_phase,
                        );
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

// ── translate_end_turn_events ─────────────────────────────────────────────────

/// Translate engine events from `Action::EndTurn` into `CombatLog` entries
/// and ECS side-effects.
///
/// - `TurnEnded`     → `CombatEvent::TurnEnded`
/// - `TurnSkipped`   → `CombatEvent::TurnSkipped`
/// - `TurnStarted`   → `CombatEvent::TurnStarted` + `ActiveCombatant` insert (mid-round only)
/// - `RoundStarted`  → `CombatEvent::RoundStarted` + `NextState<CombatPhase>::StartRound`
/// - `AuraStatusGained`/`Lost` → `CombatEvent::StatusApplied`/`StatusExpired`
/// - All other events forwarded to `translate_tick_events`.
fn translate_end_turn_events(
    events: &[Event],
    id_map: &UnitIdMap,
    commands: &mut Commands,
    log: &mut CombatLog,
    next_phase: &mut Option<ResMut<NextState<CombatPhase>>>,
) {
    let mut round_started = false;

    for ev in events {
        match ev {
            Event::TurnEnded { actor } => {
                if let Some(ent) = id_map.get_entity(*actor) {
                    // Mirror previous advance_turn_system: drop ActiveCombatant on old actor (lost in Phase 4e sweep)
                    commands.entity(ent).remove::<ActiveCombatant>();
                    log.push(CombatEvent::TurnEnded { actor: ent });
                }
            }
            Event::TurnSkipped { actor, .. } => {
                if let Some(ent) = id_map.get_entity(*actor) {
                    commands.entity(ent).remove::<ActiveCombatant>();
                    log.push(CombatEvent::TurnSkipped { actor: ent });
                }
            }
            Event::RoundStarted { round } => {
                log.push(CombatEvent::RoundStarted { round: *round });
                if let Some(ref mut np) = next_phase {
                    np.set(CombatPhase::StartRound);
                }
                round_started = true;
            }
            Event::TurnStarted { actor } => {
                if let Some(ent) = id_map.get_entity(*actor) {
                    if !round_started {
                        // Mid-round handoff: insert ActiveCombatant on the new actor.
                        // After RoundStarted, build_turn_order inserts it on re-entry.
                        commands.entity(ent).insert(ActiveCombatant);
                    }
                    log.push(CombatEvent::TurnStarted { actor: ent });
                }
            }
            Event::AuraStatusGained { target, status_id, .. } => {
                if let Some(tgt_ent) = id_map.get_entity(*target) {
                    log.push(CombatEvent::StatusApplied {
                        target: tgt_ent,
                        status: status_id.clone(),
                    });
                }
            }
            Event::AuraStatusLost { target, status_id, .. } => {
                if let Some(tgt_ent) = id_map.get_entity(*target) {
                    log.push(CombatEvent::StatusExpired {
                        target: tgt_ent,
                        status: status_id.clone(),
                    });
                }
            }
            other => {
                translate_tick_events(std::slice::from_ref(other), id_map, commands, log);
            }
        }
    }
}

// ── translate_cast_events ─────────────────────────────────────────────────────

/// Translate the engine `Event` stream from a single `Action::Cast` into
/// Bevy-land side effects (CombatLog entries, Dead markers).
///
/// Crit-fail handling: `Event::CritFailed` is translated to
/// `CombatEvent::CriticalMiss` (for `Miss` outcome) or
/// `CombatEvent::CritFailSideEffect` (for `DoubleCost`, `SelfDamage`,
/// `ApplyStatus` outcomes).
#[allow(clippy::too_many_arguments)]
fn translate_cast_events(
    actor: Entity,
    ability: &crate::core::AbilityId,
    target: Entity,
    target_pos: hexx::Hex,
    mana_before: Option<i32>,
    events: &[Event],
    id_map: &mut UnitIdMap,
    combat_state: &CombatStateRes,
    active_content: &ActiveContent,
    commands: &mut Commands,
    log: &mut CombatLog,
    positions: &mut HexPositions,
    visuals: &VisualAssets,
) {
    // Emit AbilityUsed from content lookup; fall back to id-as-name if absent.
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

    for ev in events {
        match ev {
            Event::UnitDamaged { target: tgt_uid, raw, mitigation, amount, .. } => {
                let Some(tgt_ent) = id_map.get_entity(*tgt_uid) else { continue };
                log.push(CombatEvent::DamageResult {
                    target: tgt_ent,
                    raw: raw.round() as i32,
                    armor_reduced: *mitigation,
                    final_damage: *amount,
                });
            }
            Event::UnitHealed { target: tgt_uid, amount } => {
                let Some(tgt_ent) = id_map.get_entity(*tgt_uid) else { continue };
                log.push(CombatEvent::HealResult {
                    target: tgt_ent,
                    formula: "engine".into(),
                    amount: *amount,
                });
            }
            Event::StatusApplied { target: tgt_uid, status } => {
                if let Some(tgt_ent) = id_map.get_entity(*tgt_uid) {
                    log.push(CombatEvent::StatusApplied {
                        target: tgt_ent,
                        status: status.clone(),
                    });
                }
            }
            Event::StatusRemoved { target: tgt_uid, status } => {
                if let Some(tgt_ent) = id_map.get_entity(*tgt_uid) {
                    log.push(CombatEvent::StatusExpired {
                        target: tgt_ent,
                        status: status.clone(),
                    });
                }
            }
            Event::UnitDied { unit } => {
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::UnitDied { entity: ent });
                    commands.entity(ent).insert(Dead);
                }
            }
            Event::RageGained { unit, current, max } => {
                if let Some(ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::RageGained {
                        actor: ent,
                        current: *current,
                        max: *max,
                    });
                }
            }
            Event::CritFailed { actor: actor_uid, outcome } => {
                let Some(actor_ent) = id_map.get_entity(*actor_uid) else { continue };
                match outcome {
                    combat_engine::CritFailOutcome::Miss => {
                        log.push(CombatEvent::CriticalMiss { actor: actor_ent });
                    }
                    combat_engine::CritFailOutcome::DoubleCost => {
                        log.push(CombatEvent::CritFailSideEffect {
                            actor: actor_ent,
                            effect_name: "mana_overload".into(),
                        });
                    }
                    combat_engine::CritFailOutcome::SelfDamage(_) => {
                        log.push(CombatEvent::CritFailSideEffect {
                            actor: actor_ent,
                            effect_name: "circuit_breach".into(),
                        });
                    }
                    combat_engine::CritFailOutcome::ApplyStatus(status_id) => {
                        log.push(CombatEvent::CritFailSideEffect {
                            actor: actor_ent,
                            effect_name: status_id.0.clone(),
                        });
                    }
                }
            }
            Event::UnitSpawned { uid, summoner: summoner_uid, pos, template_id, team } => {
                let Some(summoner_entity) = id_map.get_entity(*summoner_uid) else { continue };
                spawn_ecs_entity_from_engine_unit(
                    *uid,
                    summoner_entity,
                    *pos,
                    template_id,
                    *team,
                    commands,
                    id_map,
                    positions,
                    active_content,
                    &visuals.tag_cache,
                    &visuals.mats,
                    &visuals.token_mesh,
                    &visuals.grid_offset,
                    log,
                );
            }
            Event::SpawnBlocked { summoner: summoner_uid, template_id, reason } => {
                let Some(summoner_entity) = id_map.get_entity(*summoner_uid) else { continue };
                let reason_text = match reason {
                    combat_engine::SpawnBlockedReason::TemplateMissing => {
                        format!("шаблон '{}' не найден", template_id)
                    }
                    combat_engine::SpawnBlockedReason::MaxActiveReached => {
                        "лимит призванных достигнут".to_string()
                    }
                    combat_engine::SpawnBlockedReason::NoFreePosition => {
                        "рядом нет свободной клетки".to_string()
                    }
                };
                log.push(CombatEvent::SummonBlocked {
                    summoner: summoner_entity,
                    reason: reason_text,
                });
            }
            Event::ReactionFired { .. }
            | Event::UnitMoved { .. }
            | Event::ActionStarted { .. }
            | Event::ActionFinished { .. }
            | Event::ManaRegenerated { .. }
            | Event::EnergyRegenerated { .. }
            | Event::StatusTicked { .. }
            // Turn/round/aura events not produced by Cast.
            // PhaseEntered: ECS writes handled at caller level (apply_phase_ecs_writes).
            | Event::TurnEnded { .. }
            | Event::TurnStarted { .. }
            | Event::TurnSkipped { .. }
            | Event::RoundStarted { .. }
            | Event::AuraStatusGained { .. }
            | Event::AuraStatusLost { .. }
            | Event::PhaseEntered { .. } => {}
        }
    }

    // ManaChanged: diff before/after. Emit one entry if mana changed.
    if let Some(actor_uid) = id_map.get_id(actor) {
        if let Some(unit) = combat_state.0.unit(actor_uid) {
            if let Some((current, max)) = unit.mana {
                if mana_before != Some(current) {
                    log.push(CombatEvent::ManaChanged { actor, current, max });
                }
            }
        }
    }
}

/// Translate the engine `Event` stream from a single `Action::Move` into
/// Bevy-land side effects.
///
/// Corresponds to the side effects emitted by `movement_system`:
/// - `CombatEvent::OpportunityAttack` per AoO that fired.
/// - `CombatEvent::RageGained` for both the attacker and victim of each AoO.
/// - `CombatEvent::UnitDied` if the mover dies mid-path.
/// - `CombatEvent::UnitMoved` (single, aggregated from all per-step moves).
/// - `Dead` component inserted on the mover if they died.
/// - `PendingAnim::Movement` enqueued to `AnimationQueue`.
#[allow(clippy::too_many_arguments)]
fn translate_move_events(
    actor: Entity,
    events: &[Event],
    id_map: &UnitIdMap,
    combat_state: &CombatStateRes,
    commands: &mut Commands,
    log: &mut CombatLog,
    anim_queue: &mut AnimationQueue,
    grid_offset: &HexGridOffset,
    tokens: &Query<(Entity, &UnitToken)>,
) {
    let mut first_from: Option<hexx::Hex> = None;
    let mut last_to: Option<hexx::Hex> = None;
    let mut waypoints: Vec<Vec2> = Vec::new();
    // Most-recent ReactionFired target (decision 6.3: ReactionFired immediately
    // precedes its Damage in the event stream).
    let mut pending_aoo_target: Option<UnitId> = None;

    for ev in events {
        match ev {
            Event::UnitMoved { from, to, .. } => {
                if first_from.is_none() {
                    first_from = Some(*from);
                    waypoints.push(LAYOUT.hex_to_world_pos(*from) + grid_offset.0);
                }
                last_to = Some(*to);
                waypoints.push(LAYOUT.hex_to_world_pos(*to) + grid_offset.0);
            }
            Event::ReactionFired {
                kind: ReactionKind::OpportunityAttack,
                against,
                ..
            } => {
                pending_aoo_target = Some(*against);
            }
            Event::UnitDamaged { target, amount, source, .. } => {
                // Pair with the most recent ReactionFired (decision 6.3).
                if pending_aoo_target == Some(*target) {
                    let Some(attacker_ent) = id_map.get_entity(*source) else {
                        pending_aoo_target = None;
                        continue;
                    };
                    let Some(target_ent) = id_map.get_entity(*target) else {
                        pending_aoo_target = None;
                        continue;
                    };
                    let killed = combat_state
                        .0
                        .unit(*target)
                        .map(|u| !u.is_alive())
                        .unwrap_or(false);
                    log.push(CombatEvent::OpportunityAttack {
                        attacker: attacker_ent,
                        target: target_ent,
                        damage: *amount,
                        killed,
                    });
                    pending_aoo_target = None;
                }
                // Non-AoO damage on Move is not possible — silently ignore.
            }
            Event::RageGained { unit, current, max } => {
                if let Some(actor_ent) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::RageGained {
                        actor: actor_ent,
                        current: *current,
                        max: *max,
                    });
                }
            }
            Event::UnitDied { unit } => {
                if let Some(entity) = id_map.get_entity(*unit) {
                    log.push(CombatEvent::UnitDied { entity });
                    commands.entity(entity).insert(Dead);
                }
            }
            // Heal / status / crit-fail / spawn / turn events not produced by Move.
            // No-op pins for exhaustiveness.
            Event::UnitHealed { .. }
            | Event::StatusApplied { .. }
            | Event::StatusRemoved { .. }
            | Event::StatusTicked { .. }
            | Event::CritFailed { .. }
            | Event::UnitSpawned { .. }
            | Event::SpawnBlocked { .. } => {}
            Event::ActionStarted { .. } | Event::ActionFinished { .. } => {}
            Event::ManaRegenerated { .. } | Event::EnergyRegenerated { .. } => {}
            // Turn/round/aura events not produced by Move.
            // PhaseEntered: AoO damage handled at caller level (apply_phase_ecs_writes).
            Event::TurnEnded { .. }
            | Event::TurnStarted { .. }
            | Event::TurnSkipped { .. }
            | Event::RoundStarted { .. }
            | Event::AuraStatusGained { .. }
            | Event::AuraStatusLost { .. }
            | Event::PhaseEntered { .. } => {}
        }
    }

    // Emit single aggregated UnitMoved.
    if let (Some(from), Some(to)) = (first_from, last_to) {
        log.push(CombatEvent::UnitMoved { actor, from, to });
    }

    // Enqueue movement animation if there were any move steps.
    if !waypoints.is_empty() {
        if let Some((token_entity, _)) = tokens.iter().find(|(_, t)| t.0 == actor) {
            anim_queue.0.push_back(PendingAnim::Movement {
                token: token_entity,
                waypoints,
            });
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
/// - `pos`              → `HexPositions`
/// - `hp`               → `Vital.hp`
/// - `movement_points`  → `ActionPoints.movement_points`
/// - `reactions_left`   → `Reactions.remaining`
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
pub fn project_state_to_ecs(
    mut commands: Commands,
    combat_state: Res<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    mut positions: ResMut<HexPositions>,
    mut combatants: Query<ProjectionRow, With<Combatant>>,
) {
    for unit in combat_state.0.units() {
        let Some(entity) = id_map.get_entity(unit.id) else {
            // Unit not yet mapped to ECS — skip silently.
            continue;
        };

        // Write position.
        positions.insert(entity, unit.pos);

        // Write Vital / ActionPoints / Reactions / Rage / Mana / Energy / StatusEffects.
        if let Ok((mut vital, mut ap, mut reactions, has_bonus, rage_opt, mana_opt, energy_opt, status_effects_opt)) =
            combatants.get_mut(entity)
        {
            vital.hp = unit.hp;
            ap.action_points = unit.action_points;
            ap.movement_points = unit.movement_points;
            reactions.remaining = unit.reactions_left as u8;

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

// ── init_state_from_ecs system ────────────────────────────────────────────────

/// Initialise engine `CombatState` from the current ECS snapshot.
///
/// Called on `OnEnter(CombatPhase::AwaitCommand)` once per round.
/// After 5c.1: also populates `Unit.caster_context`, `Unit.auras`,
/// `Unit.enemy_phases` from the corresponding ECS components.
pub fn init_state_from_ecs(
    combatants: Query<CombatantRow, With<Combatant>>,
    positions: Res<HexPositions>,
    combat_context: Res<CombatContext>,
    ecs_queue: Res<TurnQueue>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    // Extra queries for the new per-combat Unit fields (5c.1).
    caster_q: Query<(Entity, &Equipment, &CombatStats, Option<&CombatPath>), With<Combatant>>,
    aoo_q: Query<(Entity, &Equipment, &CombatStats, &Abilities, Has<Dead>), With<Combatant>>,
    aura_q: Query<(Entity, &AuraSource), Without<Dead>>,
    phases_q: Query<(Entity, &EnemyPhases), With<Combatant>>,
    active_content: Res<ActiveContent>,
    // B2: run exactly once per combat session (guard against round-wrap re-init).
    //
    // Uses `Local<Option<Option<Entity>>>` to distinguish "never run" (outer None)
    // from "last seen encounter" (Some(inner)), where the inner value may itself
    // be None when no encounter entity is set (e.g. test harnesses).
    mut last_encounter: Local<Option<Option<Entity>>>,
) {
    // Skip on round 2+: if the encounter identity hasn't changed we already
    // initialised the engine state at combat start and must not overwrite it
    // (that would discard all engine-side mutations — status ticks, AP/MP
    // refills, etc.).
    let current_encounter = combat_context.encounter;
    if *last_encounter == Some(current_encounter) {
        return;
    }
    *last_encounter = Some(current_encounter);

    use crate::content::encounters::AuraAffects;

    let mut state = from_ecs(&combatants, &positions, combat_context.round, &mut id_map);

    // ── Populate new Unit fields ──────────────────────────────────────────────

    // Build caster_context for every unit (alive or dead) from Equipment + CombatStats.
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
            crit_fail_outcome: crate::combat::ai::plan::sim::map_crit_fail_effect(&crit_fail_outcome),
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.caster_context = engine_ctx;
        }
    }

    // Build aoo_dice for alive units with a melee WeaponAttack ability + weapon.
    // Mirrors the pre-5c.1 `build_ecs_content_view` AoO eligibility filter so
    // that ranged units (no melee ability) don't AoO even though they have a
    // weapon equipped. Strength modifier is baked into the dice bonus here so
    // the engine's `unit_aoo_dice` returns the final damage formula directly.
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

    // Build auras from AuraSource components (alive sources only).
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

    // Build enemy_phases from EnemyPhases.pending (first entry only per unit).
    for (entity, phases) in phases_q.iter() {
        let Some(uid) = id_map.get_id(entity) else { continue };
        if let Some(unit) = state.unit_mut(uid) {
            unit.enemy_phases = phases.pending.iter().map(|phase| {
                let pct = match phase.trigger {
                    crate::content::encounters::PhaseTrigger::HpBelowPct(p) => p,
                };
                let new_max_hp = phase.stats.as_ref().map(|s| s.max_hp).unwrap_or(0);
                combat_engine::PhaseEntry { pct, new_max_hp, heal_to_full: phase.heal_to_full }
            }).collect();
        }
    }

    // ── Populate the engine turn queue from the ECS Res<TurnQueue> ───────────
    let uid_order: Vec<UnitId> = ecs_queue
        .order
        .iter()
        .filter_map(|e| id_map.get_id(*e))
        .collect();
    state.set_turn_queue(uid_order, ecs_queue.index);

    combat_state.0 = state;
}

/// Fires once at the start of round 1 to give the first actor their full turn-start
/// treatment (AP/MP refill, mana/energy regen, status tick).
///
/// On round 2+, this is handled by the engine cascade: `step(EndTurn)` calls
/// `start_actor_turn` for the incoming actor as part of the event stream, and
/// `process_action_system` + `project_state_to_ecs` propagate it back to ECS.
///
/// Runs in `OnEnter(CombatPhase::AwaitCommand)` chained after `init_state_from_ecs`.
pub fn engine_start_first_turn_system(
    combat_context: Res<CombatContext>,
    id_map: Res<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    active_content: Res<ActiveContent>,
    mut commands: Commands,
    mut log: ResMut<CombatLog>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
) {
    if combat_context.round != 1 {
        return;
    }
    let Some(first_actor) = combat_state.0.turn_queue.current() else { return };

    let content = build_ecs_content_view(&active_content);
    let events = combat_state.0.start_actor_turn(first_actor, &content);

    translate_tick_events(&events, &id_map, &mut commands, &mut log);

    // Queue ECS-only phase deltas (same pattern as process_action_system).
    for ev in &events {
        if let Event::PhaseEntered { unit, phase_idx, .. } = ev {
            pending_phases.0.push((*unit, *phase_idx));
        }
    }
}

