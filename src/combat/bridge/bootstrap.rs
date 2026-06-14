//! ECS → engine bootstrap: `from_ecs`, `build_unit`, `bootstrap_combat_state`.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::content::races::CritFailEffect;
use crate::game::combat_log::CombatLog;
use crate::game::components::{
    Abilities, ActionPoints, AuraSource, CombatPath, CombatStats, Combatant, Dead, EnemyPhases,
    Energy, Equipment, Faction, Mana, Rage, Reactions, Speed, StatusEffects, TemplateRef, Vital,
};
use crate::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, HexCorpses, HexPositions,
    PresetInitiative, TurnQueue,
};

use super::*;
use combat_engine::dice::DiceExpr as EngineDiceExpr;
use combat_engine::modifier;
use combat_engine::{
    content::{AuraDef, TeamRelation},
    event::Event,
    state::{ActiveStatus, CombatState, Pool, RoundPhase, Unit, UnitId},
};

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
            ranged_dice: bevy_ctx.ranged_dice,
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
                matches!(def.effect, EffectDef::WeaponAttack { ranged: false, .. })
                    && def.range.max == 1
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
        // Capture the unit's current base_speed BEFORE the mutable borrow below,
        // so the phase-equipment closure can use it as fallback without a double-borrow.
        let current_base_speed = state.unit(uid).map(|u| u.runtime.base_speed).unwrap_or(0);
        if let Some(unit) = state.unit_mut(uid) {
            unit.enemy_phases = phases
                .pending
                .iter()
                .map(|phase| {
                    let crate::content::encounters::PhaseTrigger::HpBelowPct(pct) = phase.trigger;
                    let new_max_hp = phase.stats.as_ref().map(|s| s.max_hp).unwrap_or(0);
                    // Compute RuntimeStats when the phase carries a template's equipment/speed.
                    // Uses the same active_content helpers as the base-unit armor derivation
                    // (equipment_armor / equipment_magic_resist).
                    let runtime = phase.equipment.as_ref().map(|eq_block| {
                        let phase_equipment = Equipment {
                            main_hand: Some(eq_block.main_hand.clone()),
                            off_hand: eq_block.off_hand.clone(),
                            chest: eq_block.chest.clone(),
                            legs: eq_block.legs.clone(),
                            feet: eq_block.feet.clone(),
                        };
                        combat_engine::RuntimeStats {
                            armor: active_content.equipment_armor(&phase_equipment),
                            magic_resist: active_content.equipment_magic_resist(&phase_equipment),
                            // Phase template's speed wins; fall back to the unit's
                            // current base_speed (captured before this borrow).
                            base_speed: phase.base_speed.unwrap_or(current_base_speed),
                        }
                    });
                    combat_engine::PhaseEntry {
                        pct,
                        new_max_hp,
                        heal_to_full: phase.heal_to_full,
                        tags: phase.tags.clone(),
                        runtime,
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
