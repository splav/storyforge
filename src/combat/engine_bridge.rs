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

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::app_state::CombatPhase;
use crate::combat::ai::config::role::{infer_profile, AxisProfile};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::content::races::CritFailEffect;
use crate::game::bundles::enemy_bundle;
use crate::game::combat_log::{
    CombatEvent, CombatLog, CritFailOutcomeEcs, SpawnBlockedReasonEcs, TurnSkipReasonEcs,
};
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, AiBehaviorOverride, AuraSource, BonusMovement,
    CombatPath, CombatStats, Combatant, Dead, EnemyPhases, Energy, Equipment, Faction, Mana, Rage,
    Reactions, Speed, StatusEffects, SummonedBy, Tags, TemplateRef, UnitToken, VictoryTarget,
    Vital,
};
use crate::game::hex::LAYOUT;
use crate::game::messages::{ActionInput, RestartCombat};
use crate::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective, HexCorpses,
    HexPositions, PhaseDeadline, PhaseDeadlineState, PresetInitiative, TurnQueue, UiDirty,
    UiDirtyFlags,
};
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

use combat_engine::dice::DiceExpr as EngineDiceExpr;
use combat_engine::modifier;
use combat_engine::{
    action::Action,
    content::{AuraDef, ContentView as EngineContentView, TeamRelation},
    event::Event,
    reaction::ReactionKind,
    state::{ActiveStatus, CombatState, Pool, RoundPhase, Unit, UnitId},
    step::step,
    PoolKind,
};

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
///
/// **Required:** `Vital`, `Faction` — semantically essential (no defaults).
/// **Optional (with defaults):** `Speed=0`, `ActionPoints={1,1,0}`,
/// `Reactions={1,1}`. Defaults match "minimal NPC" semantics (immobile,
/// one action, one reaction). When any default kicks in `from_ecs` emits
/// a `warn!` so the missing component is loud, not silent.
type CombatantRow<'a> = (
    Entity,
    &'a Vital,
    Option<&'a Speed>,
    Option<&'a ActionPoints>,
    Option<&'a Reactions>,
    &'a Faction,
    Option<&'a StatusEffects>,
    Option<&'a Rage>,
    Option<&'a Mana>,
    Option<&'a Energy>,
    Option<&'a TemplateRef>,
);

/// All pure (non-ECS) inputs needed to construct one engine `Unit`.
///
/// Populated by `from_ecs` after component reads; passed to `build_unit`.
/// Kept as a plain struct so a future ECS-free `init_fight` path can build
/// the same `Unit` from content/templates without touching Bevy.
pub(crate) struct UnitBuildInput {
    pub uid: combat_engine::state::UnitId,
    pub team: combat_engine::state::Team,
    pub pos: hexx::Hex,
    pub armor: i32,
    pub magic_resist: i32,
    pub base_speed: i32,
    pub reactions_max: i32,
    pub statuses: Vec<combat_engine::state::ActiveStatus>,
    pub pools: combat_engine::enum_map::EnumMap<
        combat_engine::PoolKind,
        Option<combat_engine::state::Pool>,
    >,
    pub regen_per_pool:
        combat_engine::enum_map::EnumMap<combat_engine::PoolKind, combat_engine::RegenRule>,
    pub template_id: Option<String>,
}

/// Pure `Unit` constructor — no ECS, no queries.
///
/// Runs the status-aggregate-bonus loop (mirrors `Effect::RefreshAggregates`,
/// status half only) then calls `Unit::new` with the derived values.
///
/// This function owns the single call site of `Unit::new` that was previously
/// inlined inside `from_ecs`, making it reusable for the future ECS-free
/// `init_fight` path (Step 2+).
pub(crate) fn build_unit(input: UnitBuildInput, content: &ActiveContent) -> Unit {
    // Compute status-derived aggregate bonuses from active statuses.
    // Mirrors Effect::RefreshAggregates (status half only); aura-based
    // contributions are added after bootstrap populates unit.auras.
    let mut armor_bonus: i32 = 0;
    let mut speed_bonus: i32 = 0;
    let mut damage_taken_bonus: i32 = 0;
    for s in &input.statuses {
        if let Some(def) = content.statuses.get(&s.id) {
            armor_bonus += def.engine.bonuses.armor_bonus;
            speed_bonus += def.engine.bonuses.speed_bonus;
            damage_taken_bonus += def.engine.bonuses.damage_taken_bonus;
        }
    }

    Unit::new(
        input.uid,
        input.team,
        input.pos,
        input.armor,
        input.magic_resist,
        armor_bonus,
        damage_taken_bonus,
        input.base_speed,
        input.base_speed + speed_bonus,
        // Bootstrap-initial: a unit always enters combat with a full reaction
        // budget. We intentionally ignore `Reactions.remaining` here — the ECS
        // default starts at 0 (matching `Effect::Spawn`'s reactions_left=0 for
        // mid-combat summons), so reading it would yield 0 and break round-1
        // AoO. Engine's `start_round` (called on `Effect::BumpRound`) refills
        // reactions_left = reactions_max on every subsequent round.
        input.reactions_max,
        input.reactions_max,
        input.statuses,
        None,
        None, // initiative: not yet rolled
        // Per-combat fields populated by bootstrap_combat_state after from_ecs.
        combat_engine::CasterContext::default(),
        None,
        Vec::new(),
        Vec::new(),
        input.pools,
        input.regen_per_pool,
        input.template_id,
    )
}

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
        .filter_map(
            |(
                entity,
                vital,
                speed,
                ap,
                reactions,
                faction,
                statuses,
                rage,
                mana,
                energy_opt,
                template_ref,
            )| {
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
                                // ECS-origin statuses always have a unit applier.
                                // `None` applier would mean an env-applied status was
                                // round-tripped through ECS, which does not occur in
                                // normal gameplay; map defensively to a fixed Env(0).
                                applier: match s.applier {
                                    Some(e) => {
                                        combat_engine::state::EffectSource::Unit(entity_to_uid(e))
                                    }
                                    None => combat_engine::state::EffectSource::Env(
                                        combat_engine::state::EnvId(0),
                                    ),
                                },
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let team = faction.0;

                // Dead units: keep with hp=0 (tombstone).
                let hp = if is_dead { 0 } else { vital.hp };

                // ── Fail-loud defaults for optional components ───────────────────
                // Speed / ActionPoints / Reactions are optional in `CombatantRow`
                // so minimal NPC entities (just Combatant + Faction + Vital) are
                // accepted into engine state. Missing components fall back to
                // "immobile, single-action" defaults, BUT we emit a `warn!` so
                // the gap is loud, not silent — catches forgotten components in
                // template spawns (see the wounded_scout regression).
                let speed_val = match speed {
                    Some(s) => s.0,
                    None => {
                        bevy::log::warn!(
                            "Combatant entity {:?} has no Speed — defaulting to 0",
                            entity
                        );
                        0
                    }
                };
                let (ap_cur, ap_max, mp_cur) = match ap {
                    Some(a) => (a.action_points, a.max_ap, a.movement_points),
                    None => {
                        bevy::log::warn!(
                            "Combatant entity {:?} has no ActionPoints — defaulting to (1,1,0)",
                            entity
                        );
                        (1, 1, 0)
                    }
                };
                let reactions_max = match reactions {
                    Some(r) => r.max,
                    None => {
                        bevy::log::warn!(
                            "Combatant entity {:?} has no Reactions — defaulting to 1",
                            entity
                        );
                        1
                    }
                };

                let rage_pool: Option<Pool> = rage.map(|r| (r.current, r.max));
                let mana_pool: Option<Pool> = mana.map(|m| (m.current, m.max));
                let energy_pool: Option<Pool> = energy_opt.map(|e| (e.current, e.max));
                let bridge_pools = combat_engine::enum_map::enum_map! {
                    // Stage 1 dual-write: pools[Hp] mirrors Vital hp/max_hp.
                    combat_engine::PoolKind::Hp     => Some((hp, vital.max_hp)),
                    combat_engine::PoolKind::Mana   => mana_pool,
                    combat_engine::PoolKind::Rage   => rage_pool,
                    combat_engine::PoolKind::Energy => energy_pool,
                    combat_engine::PoolKind::Ap     => Some((ap_cur, ap_max)),
                    combat_engine::PoolKind::Mp     => Some((mp_cur, mp_cur)),
                };
                let bridge_regen = combat_engine::enum_map::enum_map! {
                    // Hp has no turn-start regen in gameplay.
                    combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
                    combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
                    combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
                    combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
                    combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
                    combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
                };

                Some(build_unit(
                    UnitBuildInput {
                        uid,
                        team,
                        pos,
                        armor: vital.armor,
                        magic_resist: vital.magic_resist,
                        base_speed: speed_val,
                        reactions_max: reactions_max as i32,
                        statuses: statuses_vec,
                        pools: bridge_pools,
                        regen_per_pool: bridge_regen,
                        template_id: template_ref.map(|tr| tr.0.clone()),
                    },
                    content,
                ))
            },
        )
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

/// Consolidated bridge-side-effect queues.
///
/// Groups all four formerly-separate `Pending*` Resources that had identical
/// shape (deferred vecs drained by apply systems in the `Execute` step).
/// Producers write into the relevant sub-field; the two apply systems drain
/// their respective halves before/after `project_state_to_ecs`.
///
/// Sub-fields:
/// * `deaths`          — units to mark `Dead` (pre-projection)
/// * `turn_lifecycle`  — `ActiveCombatant` inserts/removes + round-start flag (pre-projection)
/// * `animations`      — movement animations to push into `AnimationQueue` (post-projection)
/// * `phases`          — `(UnitId, phase_idx)` phase-transition pairs (post-projection)
/// * `phase_overrides` — victory-override/deadline intents queued by phase transitions (post-projection)
/// * `env_revealed`    — true when at least one `EnvRevealed` event fired this frame (post-projection)
#[derive(Resource, Default)]
pub struct BridgeQueues {
    pub deaths: Vec<UnitId>,
    pub turn_lifecycle: BridgeTurnLifecycle,
    pub animations: Vec<PendingAnim>,
    pub phases: Vec<(UnitId, usize)>,
    pub phase_overrides: Vec<PhaseOverrideIntent>,
    /// Set to `true` when an `EnvRevealed` engine event fires this step.
    /// Consumed in `apply_bridge_queues_post_projection` to trigger `HEX_FILL`
    /// so the trap tile appears immediately after reveal.
    pub env_revealed: bool,
}

/// Turn-lifecycle sub-queue inside [`BridgeQueues`].
///
/// Previously `PendingTurnLifecycle`.  Extracted as a named sub-struct so the
/// field types remain self-documenting without a top-level Resource.
#[derive(Default)]
pub struct BridgeTurnLifecycle {
    pub remove_active: Vec<UnitId>,
    pub insert_active: Vec<UnitId>,
    /// When true, a `RoundStarted` was seen this frame; a `StartRound` transition
    /// is scheduled by `apply_bridge_queues_pre_projection`.  `insert_active` is
    /// always drained in the same call — the `BumpRound`-settled actor is inserted
    /// via `insert_active` before `build_turn_order` runs in the next StartRound frame.
    pub round_started: bool,
}

/// Deferred victory-override / deadline intent emitted when a boss phase fires.
/// Consumed by `apply_phase_overrides_system` so `apply_phase_ecs_writes` (which
/// already has a 7-tuple query) need not also take the objective/deadline resources.
pub struct PhaseOverrideIntent {
    pub entity: Entity,
    pub victory_override: Option<crate::content::encounters::VictoryCondition>,
    pub turn_limit: Option<u32>,
}

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
    let crit_fail_effect = tpl
        .path
        .as_deref()
        .and_then(|p| active_content.paths.get(p))
        .map_or(crate::content::races::CritFailEffect::Miss, |p| {
            p.crit_fail_effect.clone()
        });
    let engine_ctx = combat_engine::CasterContext {
        str_mod: bevy_ctx.str_mod,
        int_mod: bevy_ctx.int_mod,
        spell_power: bevy_ctx.spell_power,
        weapon_dice: bevy_ctx.weapon_dice,
        crit_fail_outcome: crate::content::to_engine::crit_fail_outcome(&crit_fail_effect),
        dex_mod: modifier(tpl.stats.dexterity),
    };

    // AoO dice: unit needs a melee WeaponAttack ability (range.max == 1) + weapon dice.
    let has_melee = tpl.ability_ids.iter().any(|aid| {
        active_content
            .abilities
            .get(aid)
            .is_some_and(|def| matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1)
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
        regen_per_pool: combat_engine::enum_map::enum_map! {
            // Hp has no turn-start regen in gameplay.
            combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
        initial_statuses: tpl
            .initial_statuses
            .iter()
            .map(|s| combat_engine::StatusId::from(s.as_str()))
            .collect(),
        initial_pools: {
            let map = &tpl.initial_pools;
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => map.get("hp").copied(),
                combat_engine::PoolKind::Mana   => map.get("mana").copied(),
                combat_engine::PoolKind::Rage   => map.get("rage").copied(),
                combat_engine::PoolKind::Energy => map.get("energy").copied(),
                combat_engine::PoolKind::Ap     => map.get("ap").copied(),
                combat_engine::PoolKind::Mp     => map.get("mp").copied(),
            }
        },
        tags: Default::default(),
    }
}

/// Build `EcsContentView` from the current ECS state.
///
/// After 5c.1, `EcsContentView` only wraps `ActiveContent` — all per-combat
/// state (caster contexts, auras, phase triggers) now lives on engine `Unit`
/// fields and is populated once at init by `from_ecs`.
///
/// Called from `bootstrap_combat_state`, `process_action_system`, and
/// `advance_turn_system` (for dead-actor sirota-DoT ticks).
///
/// Also used by `replay_engine_trace` to build a content view from layered
/// campaign content without going through the full Bevy ECS.
pub fn build_ecs_content_view<'a>(content: &'a ActiveContent) -> EcsContentView<'a> {
    EcsContentView {
        active_content: content,
    }
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
///   5. If the phase carries `victory_override` or `turn_limit`, pushes a
///      `PhaseOverrideIntent` into `overrides` for deferred application.
///
/// Called from `apply_bridge_queues_post_projection` which runs AFTER `project_state_to_ecs`
/// to avoid a query conflict over `&mut Vital` between the two systems.
/// `process_action_system` and `bootstrap_combat_state` record `(unit, phase_idx)`
/// pairs into `PendingPhaseTransitions`; this helper drains them.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
    overrides: &mut Vec<PhaseOverrideIntent>,
) {
    let Some(ent) = id_map.get_entity(unit) else {
        return;
    };
    let Ok((mut phases, mut vital, mut stats, mut abilities, role_opt, mut name, is_dead)) =
        q.get_mut(ent)
    else {
        return;
    };

    let Some(phase) = phases.pending.get(phase_idx).cloned() else {
        return;
    };

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
        next_name: next_name.clone(),
        flavor: phase.flavor.clone(),
    });

    // Queue victory-override/deadline intent if the phase carries either field.
    if phase.victory_override.is_some() || phase.turn_limit.is_some() {
        overrides.push(PhaseOverrideIntent {
            entity: ent,
            victory_override: phase.victory_override.clone(),
            turn_limit: phase.turn_limit,
        });
    }

    // Insert AI behavior override component if the phase specifies one.
    if let Some(kind) = phase.ai_behavior {
        commands.entity(ent).insert(AiBehaviorOverride { kind });
    }

    // Mirror tag replacement into the ECS Tags component so Bevy-side legality
    // (BevyActions / ValidationTargetQ) doesn't read stale tags after the phase.
    // The engine already replaced Unit.tags in the EnterPhase arm (Slice C1);
    // this keeps the ECS copy in sync. None = keep existing Tags component unchanged.
    if let Some(ref new_tags) = phase.tags {
        commands.entity(ent).insert(Tags(new_tags.clone()));
    }

    // Pop exactly once per event (spec §8).
    phases.pending.remove(phase_idx);
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

/// Bundles static combat-environment params to reduce bootstrap_combat_state
/// system-param count below Bevy's 16-param limit.
#[derive(SystemParam)]
pub struct EnvironmentParams<'w> {
    pub active_content: Res<'w, ActiveContent>,
    pub blocked_hexes: Res<'w, CombatBlockedHexes>,
    pub environment: Res<'w, CombatEnvironment>,
}

/// Bundles the initiative-rolling params added in Wave 3.
///
/// Groups the three new params that would push `bootstrap_combat_state` over
/// Bevy's 16-system-param limit.
#[derive(SystemParam)]
pub struct InitiativeParams<'w, 's> {
    pub rng: ResMut<'w, crate::combat::DiceRngRes>,
    pub preset: ResMut<'w, PresetInitiative>,
    pub name_q: Query<'w, 's, (Entity, &'static Name), With<Combatant>>,
}

// ── Queue Resources for deferred ECS side-effects ────────────────────────────

// ── apply-systems for the new queue Resources ─────────────────────────────────

/// Drains the pre-projection half of [`BridgeQueues`]: deaths and turn-lifecycle.
///
/// Runs after `process_action_system`, before `project_state_to_ecs`.
///
/// Turn-lifecycle drain order: `remove_active` first (evict old/skipped holder),
/// then `insert_active` (set new holder) → exactly one `ActiveCombatant` at all
/// times, no empty frame between remove and insert.
///
/// `round_started`: schedules the `StartRound` phase transition and resets the
/// flag.  `insert_active` is **always** drained — `BumpRound`'s `TurnStarted`
/// pushes the engine-settled actor into `insert_active`, and `build_turn_order`
/// no longer does a blanket `remove::<ActiveCombatant>`, so draining here is
/// safe and correct for both round-boundary and mid-round handoffs.
pub fn apply_bridge_queues_pre_projection(
    mut queues: ResMut<BridgeQueues>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut next_phase: Option<ResMut<NextState<CombatPhase>>>,
) {
    // Deaths
    for uid in std::mem::take(&mut queues.deaths) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).insert(Dead);
        }
    }

    // Turn lifecycle — remove before insert to maintain exactly-one invariant.
    for uid in std::mem::take(&mut queues.turn_lifecycle.remove_active) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).remove::<ActiveCombatant>();
        }
    }

    if queues.turn_lifecycle.round_started {
        // Schedule the StartRound phase transition; reset the flag.
        // insert_active is drained below (same path as mid-round) so the
        // BumpRound-settled actor gets ActiveCombatant before StartRound runs.
        if let Some(ref mut np) = next_phase {
            np.set(CombatPhase::StartRound);
        }
        queues.turn_lifecycle.round_started = false;
    }

    // Always drain insert_active (covers both mid-round handoff and round-boundary).
    for uid in std::mem::take(&mut queues.turn_lifecycle.insert_active) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).insert(ActiveCombatant);
        }
    }
}

/// Drains the post-projection half of [`BridgeQueues`]: animations and phase transitions.
///
/// Runs after `project_state_to_ecs`, before `flush_pending_ai_log_system`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_bridge_queues_post_projection(
    mut queues: ResMut<BridgeQueues>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut log: ResMut<CombatLog>,
    active_content: Res<ActiveContent>,
    tag_cache: Res<AbilityTagCache>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut dirty: ResMut<UiDirty>,
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
    // Animations
    for anim in std::mem::take(&mut queues.animations) {
        anim_queue.0.push_back(anim);
    }

    // EnvRevealed: trigger HEX_FILL so a newly-revealed (still-armed) trap tile
    // renders. Reserved for the reveal mechanic (e.g. a scout spotting traps);
    // firing a trap removes it, so triggering does not emit EnvRevealed.
    if std::mem::take(&mut queues.env_revealed) {
        dirty.0 |= UiDirtyFlags::HEX_FILL;
    }

    // Phase transitions — move phases out first so we can borrow phase_overrides independently.
    let transitions = std::mem::take(&mut queues.phases);
    for (unit, phase_idx) in transitions {
        apply_phase_ecs_writes(
            unit,
            phase_idx,
            &id_map,
            &mut commands,
            &mut log,
            &mut q,
            &active_content,
            &tag_cache,
            &mut queues.phase_overrides,
        );
    }
}

/// Applies victory-override / deadline intents queued by phase transitions.
/// Runs in Execute right after `apply_bridge_queues_post_projection`.
pub fn apply_phase_overrides_system(
    mut queues: ResMut<BridgeQueues>,
    mut objective: ResMut<CombatObjective>,
    mut deadline: ResMut<PhaseDeadline>,
    ctx: Res<CombatContext>,
    mut ui_dirty: ResMut<UiDirty>,
    mut commands: Commands,
) {
    for intent in std::mem::take(&mut queues.phase_overrides) {
        if let Some(ov) = intent.victory_override {
            if let crate::content::encounters::VictoryCondition::KillTarget {
                marker_color, ..
            } = &ov
            {
                // The override always targets the phasing unit itself; load-time
                // validation (`validate_scenario`) guarantees the KillTarget enemy_name
                // equals the phasing enemy's config name. KillTarget victory is
                // marker-based (see `check_combat_end`), so attach the VictoryTarget
                // marker to the phasing entity unconditionally — its `target_alive` bool
                // and the UI ring then track the new objective. (Matching by display
                // `Name` would be wrong: combat names carry a race prefix, e.g.
                // "Зверокров Страж" vs the bare config name "Страж".)
                commands.entity(intent.entity).insert(VictoryTarget {
                    marker_color: *marker_color,
                });
            }
            objective.0 = ov;
            ui_dirty.0 |= UiDirtyFlags::PHASE_HINT;
        }
        if let Some(limit) = intent.turn_limit {
            deadline.0 = Some(PhaseDeadlineState {
                phase_started_round: ctx.round,
                limit,
            });
        }
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
    let race_name = active_content
        .races
        .get(&template.race)
        .map_or("", |r| r.name.as_str());
    let display_name = if race_name.is_empty() {
        template.name.clone()
    } else {
        format!("{} {}", race_name, template.name)
    };
    let ecs_team = match team {
        combat_engine::state::Team::Player => EcsTeam::Player,
        combat_engine::state::Team::Enemy => EcsTeam::Enemy,
    };
    let role = infer_profile(
        &template.ability_ids,
        effective.max_hp,
        armor,
        active_content,
        tag_cache,
    );

    let mut ec = commands.spawn((
        Name::new(display_name.clone()),
        enemy_bundle(
            effective,
            armor,
            0, // magic_resist: spawned units have no magic_resist (template carries none)
            template.speed,
            template.ability_ids.clone(),
            equipment,
        ),
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
    /// Consolidated bridge queues: deaths and turn_lifecycle are written here
    /// during translation; animations and phases are written in post-passes
    /// directly on the `ResMut<BridgeQueues>`.
    queues: &'a mut BridgeQueues,
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
                    mv.waypoints
                        .push(LAYOUT.hex_to_world_pos(*from) + mv.grid_offset.0);
                }
                mv.last_to = Some(*to);
                mv.waypoints
                    .push(LAYOUT.hex_to_world_pos(*to) + mv.grid_offset.0);
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
        Event::UnitDamaged {
            target,
            amount,
            raw,
            mitigation,
            pierces,
            source,
        } => {
            if let Some(mv) = ctx.move_.as_mut() {
                if mv.pending_aoo_target == Some(*target) {
                    // AoO arm: source is always a unit (reactions are unit-only).
                    let source_uid = match source {
                        combat_engine::state::EffectSource::Unit(u) => *u,
                        // An Env source cannot be an AoO attacker; fall through to
                        // the non-AoO env-damage branch below.
                        combat_engine::state::EffectSource::Env(_) => {
                            // Clear pending — this is not the expected AoO damage.
                            mv.pending_aoo_target = None;
                            // Fall through to env-damage log below.
                            let armor_reduced = if *pierces { 0 } else { *mitigation };
                            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                                ctx.log.push(CombatEvent::DamageResult {
                                    target: tgt_ent,
                                    raw: raw.round() as i32,
                                    armor_reduced,
                                    final_damage: *amount,
                                });
                            }
                            return;
                        }
                    };
                    let Some(attacker_ent) = ctx.id_map.get_entity(source_uid) else {
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
                } else {
                    // Non-AoO damage during Move: only env (trap) damage reaches
                    // here.  Log it so HP/UI stay consistent; no attacker entity.
                    let armor_reduced = if *pierces { 0 } else { *mitigation };
                    if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                        ctx.log.push(CombatEvent::DamageResult {
                            target: tgt_ent,
                            raw: raw.round() as i32,
                            armor_reduced,
                            final_damage: *amount,
                        });
                    }
                }
            } else if ctx.cast.is_some() {
                // Cast context: engine already zeroes mitigation for piercing casts.
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
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
        Event::DotDamaged {
            target,
            source,
            source_status,
            raw,
            mitigation,
            pierces,
            amount,
        } => {
            // no-op: DotDamaged not produced during Cast or Move actions
            if ctx.cast.is_none() && ctx.move_.is_none() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
                // For env-applied DoTs there is no unit attacker; source is None.
                let src_ent_opt: Option<Entity> = match source {
                    combat_engine::state::EffectSource::Unit(u) => ctx.id_map.get_entity(*u),
                    combat_engine::state::EffectSource::Env(_) => None,
                };
                ctx.log.push(CombatEvent::DotDamaged {
                    target: tgt_ent,
                    source: src_ent_opt,
                    source_status: source_status.clone(),
                    raw: *raw,
                    mitigation: *mitigation,
                    pierces: *pierces,
                    amount: *amount,
                });
            }
        }

        // ── HoT heal (fused atomic, tick context only) ───────────────────────
        Event::HotHealed {
            target,
            source_status,
            amount,
        } => {
            // no-op: HotHealed not produced during Cast or Move actions
            if ctx.cast.is_none() && ctx.move_.is_none() {
                if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                    ctx.log.push(CombatEvent::HotHealed {
                        target: tgt_ent,
                        source_status: source_status.clone(),
                        amount: *amount,
                    });
                }
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
        Event::AuraStatusGained {
            target, status_id, ..
        } => {
            if let Some(tgt_ent) = ctx.id_map.get_entity(*target) {
                ctx.log.push(CombatEvent::StatusApplied {
                    target: tgt_ent,
                    status: status_id.clone(),
                });
            }
        }
        Event::AuraStatusLost {
            target, status_id, ..
        } => {
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
                ctx.queues.deaths.push(*unit);
            }
        }

        // ── Healing (cast only) ───────────────────────────────────────────────
        Event::UnitHealed { target, amount } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(tgt_ent) = ctx.id_map.get_entity(*target) else {
                    return;
                };
                ctx.log.push(CombatEvent::HealResult {
                    target: tgt_ent,
                    amount: *amount,
                });
            }
        }

        // ── Resource changes (C6: only PoolChanged remains) ──────────────────

        // ── Crit-fail (cast only) ─────────────────────────────────────────────
        Event::CritFailed {
            actor: actor_uid,
            outcome,
        } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(actor_ent) = ctx.id_map.get_entity(*actor_uid) else {
                    return;
                };
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
        Event::SpawnBlocked {
            summoner: summoner_uid,
            reason,
            ..
        } => {
            // no-op: not produced during tick or move actions
            if ctx.cast.is_some() {
                let Some(summoner_entity) = ctx.id_map.get_entity(*summoner_uid) else {
                    return;
                };
                ctx.log.push(CombatEvent::SummonBlocked {
                    summoner: summoner_entity,
                    reason: SpawnBlockedReasonEcs::from(reason),
                });
            }
        }

        // ── Turn / round lifecycle (any context after B5) ─────────────────────
        Event::TurnEnded { actor, cause } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.queues.turn_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnEnded {
                    actor: ent,
                    cause: crate::game::combat_log::TurnEndCauseEcs::from(cause),
                });
            }
        }
        Event::TurnSkipped { actor, reason } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                ctx.queues.turn_lifecycle.remove_active.push(*actor);
                ctx.log.push(CombatEvent::TurnSkipped {
                    actor: ent,
                    reason: TurnSkipReasonEcs::from(reason),
                });
            }
        }
        Event::RoundStarted { round } => {
            ctx.log.push(CombatEvent::RoundStarted { round: *round });
            ctx.queues.turn_lifecycle.round_started = true;
        }
        Event::TurnStarted { actor } => {
            if let Some(ent) = ctx.id_map.get_entity(*actor) {
                // Always queue insert_active — the engine is the sole authority
                // for whose turn it is. Works uniformly for:
                //   round 1: settle_round_start (bootstrap)
                //   round 2+: BumpRound cascade
                //   mid-round: normal EndTurn handoff
                ctx.queues.turn_lifecycle.insert_active.push(*actor);
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

        // ── Unified pool-change (C6: sole pool-mutation event) ───────────────
        Event::PoolChanged {
            unit,
            pool,
            current,
            max,
            cause,
        } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::PoolChanged {
                    actor: ent,
                    pool: *pool,
                    current: *current,
                    max: *max,
                    cause: *cause,
                });
            }
        }

        // ── Hazard / env events ────────────────────────────────────────────────
        // A trap fired (one-shot) and was removed from the board — log the hit.
        Event::HazardTriggered { victim, .. } => {
            if let Some(victim_ent) = ctx.id_map.get_entity(*victim) {
                ctx.log
                    .push(CombatEvent::HazardTriggered { victim: victim_ent });
            }
        }
        // EnvRevealed: an armed trap became visible (reveal mechanic). Flag the
        // bridge so post-projection drains it into UiDirty. Not emitted on fire.
        Event::EnvRevealed { .. } => {
            ctx.queues.env_revealed = true;
        }

        // ── Initiative rolls (round-start, dormant until Wave 5) ─────────────
        // Emitted by CombatState::roll_initiative_for_all. Not wired into the
        // round lifecycle yet; translate here so the workspace compiles and the
        // combat-log rendering path is exercised once Wave 5 emits these.
        Event::InitiativeRolled {
            unit,
            roll,
            dex_mod,
            total,
        } => {
            if let Some(ent) = ctx.id_map.get_entity(*unit) {
                ctx.log.push(CombatEvent::InitiativeRolled {
                    actor: ent,
                    dex_mod: *dex_mod,
                    roll: *roll,
                    total: *total,
                });
            }
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
#[allow(clippy::too_many_arguments)]
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
    mut queues: ResMut<BridgeQueues>,
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
                        if let Err(e) =
                            trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        // Save interrupted flag before `ctx` is shadowed by TranslateCtx below.
                        let move_was_interrupted = ctx.interrupted;
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
                                queues: &mut queues,
                                cast: None,
                                move_: Some(move_ctx),
                            };
                            translate_events(&events, &mut ctx);
                            let mv = ctx.move_.take().unwrap();
                            (mv.first_from, mv.last_to, mv.waypoints, mv.actor)
                        };
                        // Emit aggregated UnitMoved and enqueue animation (ctx dropped above).
                        if let (Some(from), Some(to)) = (final_from, final_to) {
                            log.push(CombatEvent::UnitMoved {
                                actor: final_actor,
                                from,
                                to,
                            });
                        }
                        if !final_waypoints.is_empty() {
                            if let Some((token_entity, _)) =
                                visuals.tokens.iter().find(|(_, t)| t.0 == final_actor)
                            {
                                queues.animations.push(PendingAnim::Movement {
                                    token: token_entity,
                                    waypoints: final_waypoints,
                                });
                            }
                        }
                        // AoO on a move can cross a phase threshold; queue for apply system.
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
                            }
                        }
                        // EnvRevealed post-pass: push CombatLog entry with the trap's hex.
                        // Done here (not in translate_one) because resolving the hex requires
                        // reading combat_state, which is not available inside TranslateCtx.
                        for ev in &events {
                            if let Event::EnvRevealed { env_id } = ev {
                                let hex = combat_state
                                    .0
                                    .environment
                                    .iter()
                                    .find(|e| e.id == *env_id)
                                    .map(|e| e.hex)
                                    .unwrap_or(hexx::Hex::ZERO);
                                log.push(CombatEvent::EnvRevealed { hex });
                            }
                        }
                        // Tail-drop: if this Move was interrupted (AoO, hazard reveal, trap
                        // fire, etc.), drop any remaining queued ActionInputs for this turn.
                        // A bundled Cast planned from the pre-move position must NOT fire from
                        // the truncated landing hex — the AI self-corrects by re-planning next
                        // frame.
                        if move_was_interrupted {
                            break;
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
            ActionInput::Cast {
                actor,
                ability,
                target,
                target_pos,
            } => {
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
                        if let Err(e) =
                            trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        emit_ability_used(
                            *actor,
                            ability,
                            *target,
                            *target_pos,
                            &active_content,
                            &mut log,
                        );
                        {
                            let cast_ctx = CastCtx { _phantom: () };
                            let mut ctx = TranslateCtx {
                                log: &mut log,
                                id_map: &mut id_map,
                                queues: &mut queues,
                                cast: Some(cast_ctx),
                                move_: None,
                            };
                            translate_events(&events, &mut ctx);
                        } // ctx drops here, releasing &mut id_map
                          // Post-pass: handle UnitSpawned separately (needs &mut Commands
                          // which cannot be stored in TranslateCtx — same pattern as PhaseEntered).
                        for ev in &events {
                            if let Event::UnitSpawned {
                                uid,
                                summoner: summoner_uid,
                                pos,
                                template_id,
                                team,
                            } = ev
                            {
                                let Some(summoner_entity) = id_map.get_entity(*summoner_uid) else {
                                    continue;
                                };
                                let spawned_uid = *uid;
                                if let Some(new_entity) = spawn_ecs_entity_from_engine_unit(
                                    spawned_uid,
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
                                ) {
                                    // The InitiativeRolled event for the summon was emitted
                                    // before UnitSpawned — translate_events skipped it because
                                    // the entity didn't exist yet. Push it now that it does.
                                    if let Some(Event::InitiativeRolled { roll, dex_mod, total, .. }) = events
                                        .iter()
                                        .find(|e| matches!(e, Event::InitiativeRolled { unit, .. } if *unit == spawned_uid))
                                    {
                                        log.push(CombatEvent::InitiativeRolled {
                                            actor: new_entity,
                                            dex_mod: *dex_mod,
                                            roll: *roll,
                                            total: *total,
                                        });
                                    }
                                }
                            }
                        }
                        // Queue phase transitions from cast events (most common case:
                        // boss crosses HP threshold from a direct damage spell).
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
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
                match step(
                    &mut combat_state.0,
                    end_action.clone(),
                    &mut rng.0,
                    &content,
                ) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) =
                            trace_writer.write_step(&end_action, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        let mut ctx = TranslateCtx {
                            log: &mut log,
                            id_map: &mut id_map,
                            queues: &mut queues,
                            cast: None,
                            move_: None,
                        };
                        translate_events(&events, &mut ctx);
                        // DoT ticks at end of turn can cross a phase threshold.
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
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
/// - `pools[Ap]`        → `ActionPoints.action_points` + `ActionPoints.max_ap`
/// - `pools[Mp]`        → `ActionPoints.movement_points`
/// - `reactions_left`   → `Reactions.remaining`
/// - `reactions_max`    → `Reactions.max`
/// - `pools[Rage]`      → `Rage.current`
/// - `pools[Mana]`      → `Mana.current`
/// - `pools[Energy]`    → `Energy.current`
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
///
/// **C5:** resource values sourced from `Unit.pools[PoolKind::*]` (unified pool
/// table). Legacy fields `unit.mana`, `unit.rage`, `unit.energy`,
/// `unit.action_points`, `unit.movement_points` are no longer read here;
/// they remain write-only (mirrored by C3 mutators) until C6 removes them.
pub fn project_state_to_ecs(
    mut commands: Commands,
    combat_state: Res<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    mut positions: ResMut<HexPositions>,
    mut corpses: ResMut<HexCorpses>,
    mut combatants: Query<ProjectionRow, With<Combatant>>,
    mut queue: ResMut<TurnQueue>,
) {
    for unit in combat_state.0.units() {
        let Some(entity) = id_map.get_entity(unit.id) else {
            // Unit not yet mapped to ECS — skip silently.
            continue;
        };

        if unit.hp() <= 0 {
            // Transition to corpse layer (idempotent — engine.unit.pos is stable).
            positions.remove(&entity);
            corpses.insert(entity, unit.pos);
            // Still sync hp=0 so Vital reflects death; skip AP/MP/Rage/Mana/Energy/Status.
            if let Ok((mut vital, _, _, _, _, _, _, _)) = combatants.get_mut(entity) {
                vital.hp = unit.hp();
            }
            continue;
        }

        // Alive — occupancy layer.
        positions.insert(entity, unit.pos);

        // Write Vital / ActionPoints / Reactions / Rage / Mana / Energy / StatusEffects.
        if let Ok((
            mut vital,
            mut ap,
            mut reactions,
            has_bonus,
            rage_opt,
            mana_opt,
            energy_opt,
            status_effects_opt,
        )) = combatants.get_mut(entity)
        {
            vital.hp = unit.hp();

            // AP / MP — sourced from pools[Ap] / pools[Mp] (C5).
            // Invariant: both are Some for every alive combatant.
            if let Some((ap_cur, ap_max)) = unit.pools[PoolKind::Ap] {
                ap.action_points = ap_cur;
                ap.max_ap = ap_max;
            }
            if let Some((mp_cur, _mp_max)) = unit.pools[PoolKind::Mp] {
                ap.movement_points = mp_cur;
            }

            reactions.remaining = unit.reactions_left as u8;
            reactions.max = unit.reactions_max as u8;

            if has_bonus && ap.movement_points == 0 {
                commands.entity(entity).remove::<BonusMovement>();
            }

            // Project rage.current when both sides carry a rage pool.
            if let (Some((engine_current, _engine_max)), Some(mut ecs_rage)) =
                (unit.pools[PoolKind::Rage], rage_opt)
            {
                ecs_rage.current = engine_current;
            }

            // Project mana.current when both sides carry a mana pool.
            if let (Some((current, _max)), Some(mut mana_comp)) =
                (unit.pools[PoolKind::Mana], mana_opt)
            {
                mana_comp.current = current;
            }

            // Project energy.current when both sides carry an energy pool.
            if let (Some((current, _max)), Some(mut energy_comp)) =
                (unit.pools[PoolKind::Energy], energy_opt)
            {
                energy_comp.current = current;
            }

            // Merge statuses: preserve ECS entries the engine doesn't know about.
            if let Some(mut status_effects) = status_effects_opt {
                let engine_known: std::collections::HashSet<(
                    &combat_engine::StatusId,
                    combat_engine::state::EffectSource,
                )> = unit.statuses.iter().map(|s| (&s.id, s.applier)).collect();

                // Env-applied engine statuses project to ECS with `applier: None`,
                // losing their `Env(id)` identity. Track their status *ids* so the
                // preserve filter can recognise the engine's own env statuses on the
                // ECS side. Without this, an env status (e.g. a spike-trap
                // `disoriented`) is both preserved AND re-appended from the engine
                // every frame → the list grows by one per frame and the status never
                // expires. The engine dedupes by id, so id-only matching is exact.
                let engine_env_ids: std::collections::HashSet<&combat_engine::StatusId> = unit
                    .statuses
                    .iter()
                    .filter(|s| matches!(s.applier, combat_engine::state::EffectSource::Env(_)))
                    .map(|s| &s.id)
                    .collect();

                // Preserve ECS statuses that are NOT in the engine's status list.
                // For ECS statuses with a unit applier we key on
                // `EffectSource::Unit(entity_to_uid(applier_entity))`.
                let preserved: Vec<crate::game::components::ActiveStatus> = status_effects
                    .0
                    .iter()
                    .filter(|ecs_s| {
                        match ecs_s.applier {
                            Some(applier_ent) => !engine_known.contains(&(
                                &ecs_s.id,
                                combat_engine::state::EffectSource::Unit(entity_to_uid(
                                    applier_ent,
                                )),
                            )),
                            // applier: None means env-applied. Preserve it ONLY if the
                            // engine has no matching env status — otherwise the engine
                            // re-projects it below (single source of truth), and keeping
                            // the ECS copy too would double it every frame.
                            None => !engine_env_ids.contains(&ecs_s.id),
                        }
                    })
                    .cloned()
                    .collect();

                let mut new_list: Vec<crate::game::components::ActiveStatus> = preserved;
                for engine_s in &unit.statuses {
                    // R2: map engine EffectSource back to an optional ECS Entity.
                    // EffectSource::Unit → Some(entity); EffectSource::Env → None
                    // (no unit entity represents an environment applier).
                    let applier_opt: Option<Entity> = match engine_s.applier {
                        combat_engine::state::EffectSource::Unit(uid) => {
                            Some(id_map.get_entity(uid).unwrap_or(entity))
                        }
                        combat_engine::state::EffectSource::Env(_) => None,
                    };
                    new_list.push(crate::game::components::ActiveStatus {
                        id: engine_s.id.clone(),
                        rounds_remaining: engine_s.rounds_remaining,
                        dot_per_tick: engine_s.dot_per_tick,
                        applier: applier_opt,
                    });
                }

                status_effects.0 = new_list;
            }
        }
    }

    // ── Project engine turn order + index → ECS TurnQueue ────────────────────
    // The engine owns the authoritative turn order after round-1 bootstrap.
    // On round-2+ Execute frames this keeps the UI strip in sync with the
    // engine's current cursor (turn_queue.index may advance as turns end).
    if !combat_state.0.units().is_empty() {
        queue.order = combat_state
            .0
            .turn_queue
            .order
            .iter()
            .filter_map(|uid| id_map.get_entity(*uid))
            .collect();
        queue.index = combat_state.0.turn_queue.index;
    }
}

// ── bootstrap_combat_state system ────────────────────────────────────────────

/// Bootstrap engine `CombatState` from the current ECS snapshot.
///
/// Runs at the tail of the `CombatPhase::StartRound` chain, after
/// `build_turn_order` has incremented `CombatContext.round` and cleared
/// `Reservations`.
///
/// Responsibilities:
/// - Build `CombatState` from ECS via `from_ecs` (includes V2 status-aggregate recompute).
/// - Populate per-unit `caster_context`, `aoo_dice`, `auras`, `enemy_phases`.
/// - Roll round-1 initiative via engine `roll_initiative_for_all` (in UnitId order).
/// - Reconcile turn order via `reconcile_turn_order` (stable sort by initiative).
/// - Project the engine's authoritative order back to `Res<TurnQueue>`.
/// - Prime the first actor's turn (AP/MP refill, regen, status tick).
///
/// Idempotent: returns immediately if `combat_state.0` already has units
/// (round 2+ re-entries, second-combat-in-session teardown races).
#[allow(clippy::too_many_arguments)] // Bevy ECS system: params are the DI surface.
#[allow(clippy::type_complexity)] // Bevy query tuple; factoring out hurts readability.
pub fn bootstrap_combat_state(
    combatants: Query<CombatantRow, With<Combatant>>,
    positions: Res<HexPositions>,
    corpses: Res<HexCorpses>,
    combat_context: Res<CombatContext>,
    mut queue: ResMut<TurnQueue>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    caster_q: Query<(Entity, &Equipment, &CombatStats, Option<&CombatPath>), With<Combatant>>,
    aoo_q: Query<(Entity, &Equipment, &CombatStats, &Abilities, Has<Dead>), With<Combatant>>,
    aura_q: Query<(Entity, &AuraSource), Without<Dead>>,
    phases_q: Query<(Entity, &EnemyPhases), With<Combatant>>,
    tags_q: Query<(Entity, &crate::game::components::Tags)>,
    env_params: EnvironmentParams<'_>,
    mut log: ResMut<CombatLog>,
    mut queues: ResMut<BridgeQueues>,
    mut init_params: InitiativeParams<'_, '_>,
) {
    // Idempotency guard: engine state evolves authoritatively via step() on
    // round 2+; re-importing would discard those mutations.
    if !combat_state.0.units().is_empty() {
        return;
    }

    use crate::content::encounters::AuraAffects;

    let active_content: &ActiveContent = &env_params.active_content;
    let mut state = from_ecs(
        &combatants,
        &positions,
        &corpses,
        combat_context.round,
        &mut id_map,
        active_content,
    );

    // ── Apply initial_statuses from unit templates (engine-side, idempotent) ──
    {
        let content_view = build_ecs_content_view(active_content);
        state.apply_initial_statuses(&content_view);
    }

    // ── Static obstacle hexes from encounter definition ───────────────────────
    state.blocked_hexes = env_params.blocked_hexes.0.iter().copied().collect();

    // ── Environmental objects from encounter definition ───────────────────────
    state.environment = env_params.environment.0.clone();

    // ── Populate per-unit combat fields ──────────────────────────────────────

    // caster_context: built from Equipment + CombatStats (alive and dead units).
    for (entity, equipment, stats, combat_path) in caster_q.iter() {
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
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
            dex_mod: modifier(stats.dexterity),
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
        if is_dead {
            continue;
        }
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        let has_melee = abilities.0.iter().any(|aid| {
            active_content.abilities.get(aid).is_some_and(|def| {
                matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
            })
        });
        if !has_melee {
            continue;
        }
        let ctx = CasterContext::new(stats, Some(equipment), &active_content.weapons);
        let Some(core_dice) = ctx.weapon_dice else {
            continue;
        };
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
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        let applies_to = match aura_src.affects {
            AuraAffects::Enemies => TeamRelation::Enemies,
            AuraAffects::Allies => TeamRelation::Allies,
            AuraAffects::All => TeamRelation::All,
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.auras.push(AuraDef {
                radius: aura_src.radius,
                status_id: aura_src.status.clone(),
                applies_to,
                affects_tags: aura_src.affects_tags.clone(),
            });
        }
    }

    // tags: copy BTreeSet<TagId> from Tags component into engine Unit.
    // Units without the component keep their default empty tag set.
    for (entity, tags) in tags_q.iter() {
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.tags = tags.0.clone();
        }
    }

    // enemy_phases: from EnemyPhases.pending.
    for (entity, phases) in phases_q.iter() {
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        if let Some(unit) = state.unit_mut(uid) {
            unit.enemy_phases = phases
                .pending
                .iter()
                .map(|phase| {
                    let crate::content::encounters::PhaseTrigger::HpBelowPct(pct) = phase.trigger;
                    let new_max_hp = phase.stats.as_ref().map(|s| s.max_hp).unwrap_or(0);
                    combat_engine::PhaseEntry {
                        pct,
                        new_max_hp,
                        heal_to_full: phase.heal_to_full,
                        tags: phase.tags.clone(),
                    }
                })
                .collect();
        }
    }

    // passives: filter each unit's ability list to ids whose engine def has
    // a non-empty passive trigger list; store them so resolve_turn_start_passives can fire.
    // Reuses aoo_q which already queries &Abilities — no extra system param needed.
    for (entity, _equipment, _stats, abilities, _is_dead) in aoo_q.iter() {
        let Some(uid) = id_map.get_id(entity) else {
            continue;
        };
        let passive_ability_ids: Vec<combat_engine::AbilityId> = abilities
            .0
            .iter()
            .filter(|aid| {
                active_content
                    .abilities
                    .get(*aid)
                    .is_some_and(|def| !def.passive.is_empty())
            })
            .cloned()
            .collect();
        if let Some(unit) = state.unit_mut(uid) {
            unit.passives = passive_ability_ids;
        }
    }

    // ── Roll round-1 initiative + build authoritative turn order (engine owns this) ──
    // Build preset map: Name → UnitId, only for units present in the engine state.
    // Resolve each preset name → Entity (via name_q) → UnitId (via id_map).
    let preset_map: std::collections::HashMap<UnitId, i32> = init_params
        .preset
        .0
        .iter()
        .filter_map(|(name, &val)| {
            init_params
                .name_q
                .iter()
                .find(|(_, n)| n.as_str() == name.as_str())
                .and_then(|(e, _)| id_map.get_id(e))
                .map(|uid| (uid, val))
        })
        .collect();

    let roll_events = state.roll_initiative_for_all(&mut init_params.rng.0, &preset_map);

    // Translate InitiativeRolled events into the combat log BEFORE settle,
    // so "initiative X: …" lines appear before RoundStarted/TurnStarted.
    {
        let mut tick_ctx = TranslateCtx {
            log: &mut log,
            id_map: &mut id_map,
            queues: &mut queues,
            cast: None,
            move_: None,
        };
        translate_events(&roll_events, &mut tick_ctx);
    }

    state.reconcile_turn_order();
    // reconcile_turn_order intentionally leaves index untouched; set it to 0
    // so settle_round_start starts from the head of the new order.
    state.turn_queue.index = 0;

    // ── Project engine order → ECS TurnQueue ─────────────────────────────────
    queue.order = state
        .turn_queue
        .order
        .iter()
        .filter_map(|uid| id_map.get_entity(*uid))
        .collect();
    queue.index = 0;

    // Consume preset (parity with old build_turn_order path).
    init_params.preset.0.clear();

    // ── Settle round start (skip dead/stunned, prime first valid actor) ────
    // settle_round_start emits: RoundStarted, TurnSkipped* (for each skip),
    // TurnStarted, start_actor_turn events for the settled actor.
    // translate_one(TurnStarted) → insert_active → ActiveCombatant set on
    // the correct actor after apply_bridge_queues_pre_projection drains it.
    // Only run when the order is non-empty (tests that call bootstrap directly
    // without any combatants skip this block and set ActiveCombatant manually).
    if !state.turn_queue.order.is_empty() {
        let content = build_ecs_content_view(active_content);
        let events = state.settle_round_start(&content);

        let mut tick_ctx = TranslateCtx {
            log: &mut log,
            id_map: &mut id_map,
            queues: &mut queues,
            cast: None,
            move_: None,
        };
        translate_events(&events, &mut tick_ctx);

        // Queue ECS-only phase deltas (same pattern as process_action_system).
        for ev in &events {
            if let Event::PhaseEntered {
                unit, phase_idx, ..
            } = ev
            {
                queues.phases.push((*unit, *phase_idx));
            }
        }

        // settle_round_start emitted RoundStarted (→ round_started=true) and
        // TurnStarted (→ insert_active.push(settled actor)). We are ALREADY in
        // StartRound, so clear round_started to stop apply_bridge_queues_pre_projection
        // (in AwaitCommand's Execute chain) from scheduling a spurious second
        // StartRound transition. Leave insert_active intact: that same system drains
        // it on the first AwaitCommand/Execute frame, setting ActiveCombatant on the
        // settled actor (Command no-ops that one frame, then acts). Tests that call
        // bootstrap directly (init_engine_state) invoke that drain explicitly.
        queues.turn_lifecycle.round_started = false;
    }

    combat_state.0 = state;
}

// ── reset_engine_mirrors ──────────────────────────────────────────────────────

/// Clears the engine-side mirrors (`CombatStateRes`, `UnitIdMap`,
/// `BridgeQueues`) so a fresh combat starts from a clean slate.
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
    queues: &mut BridgeQueues,
) {
    *combat_state = CombatStateRes::default();
    id_map.clear();
    *queues = BridgeQueues::default();
}

/// `OnExit(AppState::Combat)` system — natural combat-end teardown.
pub fn reset_engine_mirrors_on_exit_combat(
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut queues: ResMut<BridgeQueues>,
) {
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut queues);
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
    mut queues: ResMut<BridgeQueues>,
) {
    if reader.read().next().is_none() {
        return;
    }
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut queues);
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

        let unit = Unit::new(
            UnitId(1),
            Team::Player,
            Hex::ZERO,
            3, // armor
            0, // magic_resist
            0, // armor_bonus
            0, // damage_taken_bonus
            3, // base_speed
            3, // speed
            1, // reactions_left
            1, // reactions_max
            Vec::new(),
            None,
            None, // initiative: not yet rolled
            combat_engine::CasterContext::default(),
            None,
            Vec::new(),
            Vec::new(),
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => Some((20, 20)),
                combat_engine::PoolKind::Mana   => None,
                combat_engine::PoolKind::Rage   => None,
                combat_engine::PoolKind::Energy => None,
                combat_engine::PoolKind::Ap     => Some((1, 1)),
                combat_engine::PoolKind::Mp     => Some((3, 3)),
            },
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
                combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
            },
            None,
        );
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);

        // Mirror the production path: ApplyStatus derives RefreshAggregates.
        let (derived, _) = apply_effect(
            &mut state,
            &Effect::ApplyStatus {
                target: UnitId(1),
                status: StatusId::from("defending"),
                rounds: 1,
                dot_per_tick: 0,
                applier: combat_engine::state::EffectSource::Unit(UnitId(1)),
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
