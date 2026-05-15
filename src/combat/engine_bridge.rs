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

use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{
    Abilities, ActionPoints, BonusMovement, CombatStats, Combatant, Dead, Equipment, Faction,
    Mana, Rage, Reactions, Speed, StatusEffects, UnitToken, Vital,
};
use crate::game::hex::LAYOUT;
use crate::game::messages::ActionInput;
use crate::game::resources::{CombatContext, HexPositions};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::HexGridOffset;

use combat_engine::{
    action::Action,
    content::{ContentView as EngineContentView, StatusBonuses},
    event::Event,
    reaction::ReactionKind,
    state::{ActiveStatus, CombatState, Pool, RoundPhase, Unit, UnitId},
    step::step,
};
use combat_engine::dice::DiceExpr as EngineDiceExpr;
use combat_engine::{EffectDef as EngineEffectDef, StatusApplication as EngineStatusApplication, StatusOn as EngineStatusOn};
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
        .filter_map(|(entity, vital, speed, ap, reactions, faction, statuses, rage, mana, is_dead)| {
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

            Some(Unit {
                id: uid,
                team,
                pos,
                hp,
                max_hp: vital.max_hp,
                armor: vital.armor,
                armor_bonus: 0,           // Phase 0: status bonuses deferred to step 8+
                base_speed: speed.0,
                speed: speed.0,           // Phase 0: status speed_bonus deferred to step 8+
                action_points: ap.action_points,
                movement_points: ap.movement_points,
                reactions_left: reactions.remaining as i32,
                statuses: statuses_vec,
                rage: rage_pool,
                mana: mana_pool,
                energy: None,             // Phase 0: Energy component deferred to step 8+
            })
        })
        .collect();

    CombatState::new(units, round, RoundPhase::ActorTurn, 0)
}

// ── process_action_system ─────────────────────────────────────────────────────

/// ECS-backed `ContentView` adapter for `process_action_system`.
///
/// Built once per `ActionInput::Move` from the current ECS state.  Holds a
/// pre-computed map of eligible AoO attackers (unit → weapon dice expr).
///
/// Eligibility filter (mirrors the deleted `movement_system`'s provoker scan):
/// - alive (`!Has<Dead>` AND `vital.is_alive()`)
/// - not stunned (no status with `skips_turn = true`)
/// - has a melee `WeaponAttack` ability (`range.max == 1`)
/// - has an equipped weapon (`CasterContext::weapon_dice` is `Some`)
///
/// Team filtering is intentionally omitted here: `scan_reactions` already
/// pre-filters to enemies of the mover before consulting `aoo_dice`, so the
/// adapter does not need to know who the actor is (option B).
///
/// `status_bonuses` returns zeros — wired fully in step 4c+.
pub struct EcsContentView<'a> {
    aoo_per_unit: HashMap<UnitId, EngineDiceExpr>,
    caster_contexts: HashMap<UnitId, combat_engine::CasterContext>,
    active_content: &'a ActiveContent,
}

impl<'a> EngineContentView for EcsContentView<'a> {
    fn aoo_dice(&self, attacker: UnitId) -> Option<EngineDiceExpr> {
        self.aoo_per_unit.get(&attacker).copied()
    }

    fn status_bonuses(&self, _id: &combat_engine::StatusId) -> StatusBonuses {
        StatusBonuses::default()
    }

    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<combat_engine::AbilityDef> {
        let def = self.active_content.abilities.get(id)?;
        Some(combat_engine::AbilityDef {
            key: def.key.clone(),
            cost_ap: def.cost_ap,
            costs: def
                .costs
                .iter()
                .map(|c| combat_engine::Cost { resource: c.resource, amount: c.amount })
                .collect(),
            range: combat_engine::AbilityRange { min: def.range.min, max: def.range.max },
            target_type: match def.target_type {
                crate::content::abilities::TargetType::SingleEnemy => combat_engine::TargetType::SingleEnemy,
                crate::content::abilities::TargetType::SingleAlly => combat_engine::TargetType::SingleAlly,
                crate::content::abilities::TargetType::Myself => combat_engine::TargetType::Myself,
                crate::content::abilities::TargetType::Ground => combat_engine::TargetType::Ground,
            },
            aoe: match def.aoe {
                crate::content::abilities::AoEShape::None => combat_engine::AoEShape::None,
                crate::content::abilities::AoEShape::Circle { radius } => combat_engine::AoEShape::Circle { radius },
                crate::content::abilities::AoEShape::Line { length } => combat_engine::AoEShape::Line { length },
            },
            friendly_fire: def.friendly_fire,
            effect: match &def.effect {
                EffectDef::None => EngineEffectDef::None,
                EffectDef::WeaponAttack => EngineEffectDef::WeaponAttack,
                EffectDef::Damage { dice } => EngineEffectDef::Damage { dice: *dice },
                EffectDef::SpellDamage { dice } => EngineEffectDef::SpellDamage { dice: *dice },
                EffectDef::Heal { dice } => EngineEffectDef::Heal { dice: *dice },
                EffectDef::GrantMovement { distance } => EngineEffectDef::GrantMovement { distance: *distance },
                EffectDef::RestoreResources => EngineEffectDef::RestoreResources,
                // Summon: deferred to Phase 3. ToggleMoveMode: UI-only.
                // Both map to None for Phase 2 — Cast resolution stays in resolution.rs until step 10.
                EffectDef::Summon { .. } | EffectDef::ToggleMoveMode => EngineEffectDef::None,
            },
            statuses: def.statuses.iter().map(|s| EngineStatusApplication {
                status: s.status.clone(),
                duration_rounds: s.duration_rounds,
                on: match s.on {
                    crate::content::abilities::StatusOn::Target => EngineStatusOn::Target,
                    crate::content::abilities::StatusOn::MySelf => EngineStatusOn::MySelf,
                },
            }).collect(),
        })
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<combat_engine::StatusDef> {
        let def = self.active_content.statuses.get(id)?;
        Some(combat_engine::StatusDef {
            causes_disadvantage: def.causes_disadvantage,
            blocks_mana_abilities: def.blocks_mana_abilities,
            forces_targeting: def.forces_targeting,
            skips_turn: def.skips_turn,
            armor_bonus: def.armor_bonus,
            damage_taken_bonus: def.damage_taken_bonus,
            speed_bonus: def.speed_bonus,
        })
    }

    fn caster_context(&self, actor: UnitId) -> combat_engine::CasterContext {
        self.caster_contexts.get(&actor).cloned().unwrap_or_default()
    }
}

/// Query row for building `EcsContentView`.
type AooRow<'a> = (
    Entity,
    &'a Equipment,
    &'a CombatStats,
    &'a Abilities,
    &'a Vital,
    Option<&'a StatusEffects>,
    &'a Reactions,
    Has<Dead>,
);

/// Build `EcsContentView` from the current ECS state.
///
/// Called once per processed `ActionInput::Move` inside `process_action_system`.
fn build_ecs_content_view<'a>(
    combatants: &Query<AooRow, With<Combatant>>,
    id_map: &UnitIdMap,
    content: &'a ActiveContent,
) -> EcsContentView<'a> {
    let mut aoo_per_unit = HashMap::new();
    let mut caster_contexts = HashMap::new();

    for (entity, equipment, stats, abilities, vital, statuses, reactions, is_dead) in
        combatants.iter()
    {
        // Build caster context for every unit (alive or dead) so Cast fanout
        // can always resolve damage formulas for the actor.
        let bevy_ctx = CasterContext::new(stats, Some(equipment), &content.weapons);
        let engine_ctx = combat_engine::CasterContext {
            str_mod: bevy_ctx.str_mod,
            int_mod: bevy_ctx.int_mod,
            spell_power: bevy_ctx.spell_power,
            weapon_dice: bevy_ctx.weapon_dice,
            // TODO(unisim phase2 step 7): wire CritFailEffect from race/path content.
            // Requires resolving unit → PathDef → CritFailEffect via ActiveContent + Race
            // components.  Defaulting to Miss until that lookup is wired.
            crit_fail_outcome: Default::default(),
        };
        if let Some(uid) = id_map.get_id(entity) {
            caster_contexts.insert(uid, engine_ctx);
        }

        // AoO eligibility filters (unchanged).

        // Filter 1: alive.
        if is_dead || !vital.is_alive() {
            continue;
        }
        // Filter 2: not stunned.
        let stunned = statuses
            .map(|se| {
                se.0.iter().any(|s| {
                    content
                        .statuses
                        .get(&s.id)
                        .is_some_and(|d| d.skips_turn)
                })
            })
            .unwrap_or(false);
        if stunned {
            continue;
        }
        // Filter 3: has melee WeaponAttack ability (range.max == 1).
        let has_melee = abilities.0.iter().any(|aid| {
            content.abilities.get(aid).is_some_and(|def| {
                matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
            })
        });
        if !has_melee {
            continue;
        }
        // Filter 4: reactions remaining (engine re-checks this, but early-out is cheap).
        if reactions.remaining == 0 {
            continue;
        }
        // Filter 5: equipped weapon provides dice.
        let ctx = CasterContext::new(stats, Some(equipment), &content.weapons);
        let Some(core_dice) = ctx.weapon_dice else {
            continue;
        };
        // Map entity → UnitId and record dice.
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        let engine_dice = EngineDiceExpr::new(
            core_dice.count,
            core_dice.sides,
            core_dice.bonus + modifier(stats.strength),
        );
        aoo_per_unit.insert(uid, engine_dice);
    }

    EcsContentView { aoo_per_unit, caster_contexts, active_content: content }
}


/// `Update` system — authoritative move handler via `combat_engine::step()`.
///
/// Reads `ActionInput::Move` messages, calls `step()` against the mirrored
/// `CombatStateRes`, and translates the resulting `Event` stream into Bevy-land
/// side effects (CombatLog entries, Dead markers, movement animations).
/// The engine is the sole owner of `Action::Move` after Phase 1 — `movement_system`
/// has been deleted.
///
/// Wired with a real ECS-backed `EcsContentView` so the engine can fire AoO
/// reactions correctly.  `project_state_to_ecs` (chained immediately after)
/// writes the engine mutations back to ECS components.
///
/// Runs in `CombatStep::Execute`, gated by `CombatPhase::AwaitCommand`.
///
/// TODO(unisim phase2 step 10) — multi-frame projector clobber.
/// Until `apply_effects_system` deletes (step 10), this system and
/// `apply_effects_system` are dual-writers for `Vital.hp` / `Rage.current`
/// in ECS.  Within a round, `init_state_from_ecs` runs once
/// (`OnEnter(AwaitCommand)`) so `combat_state` keeps the round-start hp/rage
/// for every unit it doesn't itself mutate.  The projector then writes those
/// stale values every frame, reverting `apply_effects_system`'s damage on
/// the very next frame.  Visible symptom: AI debug shows full HP across
/// rounds, log records damage that never lands.  Self-resolves when step 10
/// deletes `apply_effects_system` and engine becomes sole writer.  See
/// `docs/ai/rework/step_unisim2_plan.md` §8 "Known issues".
pub fn process_action_system(
    mut commands: Commands,
    mut reader: MessageReader<ActionInput>,
    id_map: Res<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    combatants: Query<AooRow, With<Combatant>>,
    active_content: Res<ActiveContent>,
    mut rng: ResMut<crate::combat::DiceRngRes>,
    mut log: ResMut<CombatLog>,
    mut anim_queue: ResMut<AnimationQueue>,
    grid_offset: Res<HexGridOffset>,
    tokens: Query<(Entity, &UnitToken)>,
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

                let content = build_ecs_content_view(&combatants, &id_map, &active_content);

                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok(events) => {
                        translate_move_events(
                            *actor,
                            &events,
                            &id_map,
                            &combat_state,
                            &mut commands,
                            &mut log,
                            &mut anim_queue,
                            &grid_offset,
                            &tokens,
                        );
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
                // Phase 2 step 7a: routing only — engine consumes the Cast
                // and mutates CombatStateRes; event translation lands in
                // step 7b (translate_cast_events).  Production callers
                // do not write ActionInput::Cast yet — UseAbility →
                // resolve_action_system → apply_effects_system remains
                // the live path until step 9 flip.
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

                let content = build_ecs_content_view(&combatants, &id_map, &active_content);

                let mana_before = combat_state
                    .0
                    .unit(actor_uid)
                    .and_then(|u| u.mana)
                    .map(|(c, _)| c);

                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok(events) => {
                        translate_cast_events(
                            *actor,
                            ability,
                            *target,
                            *target_pos,
                            mana_before,
                            &events,
                            &id_map,
                            &combat_state,
                            &active_content,
                            &mut commands,
                            &mut log,
                        );
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: Cast step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                    }
                }
            }
        }
    }
}

/// Translate the engine `Event` stream from a single `Action::Cast` into
/// Bevy-land side effects (CombatLog entries, Dead markers).
///
/// Mirrors the side-effects `apply_effects_system` + `resolve_action_system`
/// emit on the legacy path so tests reading `CombatLog` see equivalent
/// output once the live caster writes `ActionInput::Cast` (step 9 flip).
#[allow(clippy::too_many_arguments)]
fn translate_cast_events(
    actor: Entity,
    ability: &crate::core::AbilityId,
    target: Entity,
    target_pos: hexx::Hex,
    mana_before: Option<i32>,
    events: &[Event],
    id_map: &UnitIdMap,
    combat_state: &CombatStateRes,
    active_content: &ActiveContent,
    commands: &mut Commands,
    log: &mut CombatLog,
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
            Event::UnitDamaged { target: tgt_uid, amount, source: _src_uid } => {
                let Some(tgt_ent) = id_map.get_entity(*tgt_uid) else { continue };
                let armor_reduced = combat_state
                    .0
                    .unit(*tgt_uid)
                    .map(|u| u.armor + u.armor_bonus)
                    .unwrap_or(0);
                log.push(CombatEvent::DamageResult {
                    target: tgt_ent,
                    formula: format!("engine: {}", amount),
                    armor_reduced,
                    final_damage: *amount as i32,
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
            Event::ReactionFired { .. }
            | Event::UnitMoved { .. }
            | Event::ActionStarted { .. }
            | Event::ActionFinished { .. } => {
                // No log entry — handled separately or n/a for Cast.
            }
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
            Event::UnitDamaged { target, amount, source } => {
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
                        damage: *amount as i32,
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
            // Heal / status events surface here once Phase 2 step 7 wires
            // Cast through the bridge.  Today Move actions never derive
            // these — no-op pins for exhaustiveness.
            Event::UnitHealed { .. }
            | Event::StatusApplied { .. }
            | Event::StatusRemoved { .. } => {}
            Event::ActionStarted { .. } | Event::ActionFinished { .. } => {}
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
    Option<&'a mut StatusEffects>,
);

/// `Update` system — writes engine `CombatState` back to ECS components.
///
/// Projects:
/// - `pos`              → `HexPositions`
/// - `hp`               → `Vital.hp`
/// - `movement_points`  → `ActionPoints.movement_points`
/// - `reactions_left`   → `Reactions.remaining`
/// - `rage.current`     → `Rage.current`
/// - `mana.current`     → `Mana.current`  (Phase 2 step 7c)
/// - `statuses`         → `StatusEffects` (Phase 2 step 7c, applier-aware merge)
///
/// Aura-applied statuses (written by `apply_auras_system` in TurnStart)
/// are preserved by the merge: any ECS `ActiveStatus` whose `(id, applier)`
/// pair is NOT in the engine's `unit.statuses` survives projection.  Engine
/// entries replace matching pairs and append new ones.
///
/// Additionally removes `BonusMovement` when `movement_points == 0`.
///
/// Runs immediately after `process_action_system` in the `CombatStep::Execute`
/// chain so engine mutations land in ECS in the same frame.  Engine state is
/// re-initialized from ECS only on `OnEnter(CombatPhase::AwaitCommand)` via
/// `init_state_from_ecs` (once per round, after `build_turn_order` refills),
/// so the projector is authoritative for engine-owned fields throughout each
/// round.
///
/// Unknown units (no ECS entity in `UnitIdMap`) are silently skipped.
///
/// TODO(unisim phase2 step 10) — `hp` / `rage.current` writes here race
/// against `apply_effects_system`'s writes in the same frame.  Because
/// `init_state_from_ecs` runs once per round (`OnEnter(AwaitCommand)`),
/// `combat_state.hp` stays at the round-start value for the whole round,
/// so projecting it every frame reverts non-Move damage on the next tick.
/// Resolves at step 10 when `apply_effects_system` deletes; engine becomes
/// the sole writer for `hp` / `rage` / `mana` / `statuses`.  See
/// `docs/ai/rework/step_unisim2_plan.md` §8 "Known issues".
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

        // Write Vital / ActionPoints / Reactions / Rage / Mana / StatusEffects.
        if let Ok((mut vital, mut ap, mut reactions, has_bonus, rage_opt, mana_opt, status_effects_opt)) =
            combatants.get_mut(entity)
        {
            vital.hp = unit.hp;
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

            // Merge statuses: preserve ECS entries the engine doesn't know about
            // (e.g. aura-applied entries written by `apply_auras_system` in TurnStart,
            // which runs after `init_state_from_ecs` and is therefore invisible to the
            // engine). An ECS entry is preserved when its (id, applier) pair has no
            // match in engine state; engine entries replace matching pairs.
            if let Some(mut status_effects) = status_effects_opt {
                // Build the set of (id, applier UnitId) pairs the engine knows about.
                let engine_known: std::collections::HashSet<(&combat_engine::StatusId, UnitId)> =
                    unit.statuses.iter().map(|s| (&s.id, s.applier)).collect();

                // Preserved: ECS entries whose (id, applier) are NOT in the engine set.
                let preserved: Vec<crate::game::components::ActiveStatus> = status_effects
                    .0
                    .iter()
                    .filter(|ecs_s| {
                        !engine_known.contains(&(&ecs_s.id, entity_to_uid(ecs_s.applier)))
                    })
                    .cloned()
                    .collect();

                // Projected from engine: translate engine entries to ECS shape.
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

/// Initialize / re-initialize `CombatStateRes` from current ECS state.
///
/// Wired to `OnEnter(CombatPhase::AwaitCommand)`: fires once at combat start
/// and once at the start of each round (after `build_turn_order` refills
/// MP+reactions in `StartRound`).
///
/// Engine is authoritative for state; ECS is a read-only projection.
pub fn init_state_from_ecs(
    combatants: Query<CombatantRow, With<Combatant>>,
    positions: Res<HexPositions>,
    combat_context: Res<CombatContext>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
) {
    let state = from_ecs(&combatants, &positions, combat_context.round, &mut id_map);
    combat_state.0 = state;
}
