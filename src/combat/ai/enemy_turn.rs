#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::tags::AbilityTagCache;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::repair::lifecycle as goal_lifecycle;
use crate::combat::ai::log::{AiLogger, ActorTickInput, write_actor_tick_log};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::build_snapshot;
use crate::combat::ai::intent::update_memory;
use crate::combat::ai::planning::record_committed_reservations;
use crate::combat::ai::utility::{
    pick_action, AiDecision, AiWorld, ChosenInfo,
};
use crate::content::settings::GameSettings;
use crate::core::DiceRng;
use crate::game::components::{
    ActiveCombatant, AiCombatantQ, AiCombatantQItem, Combatant, StatusEffects, Team,
};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::resources::{CombatContext, HexPositions};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use std::collections::HashMap;

// ── Bundled message writers (keeps system params under Bevy's 16-param limit) ──

#[derive(SystemParam)]
pub struct AiMessages<'w> {
    use_ability: MessageWriter<'w, UseAbility>,
    move_unit: MessageWriter<'w, MoveUnit>,
    end_turn: MessageWriter<'w, EndTurn>,
}

/// Shared read-only resources used during AI decision making. Bundling
/// everything we just *read* into one `SystemParam` slot keeps the two AI
/// systems under Bevy's 16-parameter limit.
#[derive(SystemParam)]
pub struct AiEnv<'w> {
    content: Res<'w, ActiveContent>,
    settings: Res<'w, GameSettings>,
    difficulty: Res<'w, DifficultyProfile>,
    inf_cfg: Res<'w, InfluenceConfig>,
    positions: Res<'w, HexPositions>,
    combat_ctx: Res<'w, CombatContext>,
    /// Step 9.A: tag cache for effective_ai_tags diagnostic writeback.
    ability_tags: Res<'w, AbilityTagCache>,
}

// ── Main system ────────────────────────────────────────────────────────────

pub fn enemy_ai_system(
    env: AiEnv,
    mut rng: ResMut<DiceRng>,
    mut reservations: ResMut<Reservations>,
    mut logger: ResMut<AiLogger>,
    mut msgs: AiMessages,
    mut debug_state: ResMut<AiDebugState>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<AiCombatantQ, With<Combatant>>,
    statuses: Query<&StatusEffects>,
    roles: Query<&AxisProfile>,
    mut memories: Query<&mut AiMemory>,
    names: Query<&Name>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Enemy || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    run_ai_turn(
        actor, &c, &env, &mut rng, &mut reservations,
        &mut logger, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}

/// Shared AI logic for both enemy_ai and pact_ai. Every tick re-plans from
/// scratch — there is no cross-tick plan storage. Multi-step beam search still
/// informs the choice of step[0], but the remainder of the plan is discarded
/// after each commit so subsequent ticks see actual post-action state
/// (accounts for crit-fail, misses, allies killing the target, player
/// reactions, etc.).
fn run_ai_turn(
    actor: Entity,
    c: &AiCombatantQItem,
    env: &AiEnv,
    rng: &mut DiceRng,
    reservations: &mut Reservations,
    logger: &mut AiLogger,
    msgs: &mut AiMessages,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses: &Query<&StatusEffects>,
    roles: &Query<&AxisProfile>,
    memories: &mut Query<&mut AiMemory>,
    debug_state: &mut AiDebugState,
    names: &Query<&Name>,
) {
    let content: &ContentView = &env.content;
    let settings = &env.settings;
    let difficulty = &env.difficulty;
    let inf_cfg = &env.inf_cfg;
    let positions = &env.positions;
    let combat_ctx = &env.combat_ctx;
    let ability_tags: &AbilityTagCache = &env.ability_tags;
    let Some(actor_pos) = positions.get(&actor) else {
        warn!("AI: actor {:?} has no position, ending turn", actor);
        msgs.end_turn.write(EndTurn { actor });
        return;
    };

    // Build snapshot early — needed for goal_lifecycle::pre_tick before the
    // no-AP/MP early-return path. Minor cost: snapshot built even for actors
    // that will pass immediately. Semantics are correct: TTL decay and
    // invalidating-clear must run before any early-return.
    let actor_team = c.faction.0;
    let snap = build_snapshot(
        combat_ctx.round, combatants, statuses, positions, roles, content, difficulty,
    );

    if snap.unit(actor).is_none() {
        msgs.end_turn.write(EndTurn { actor });
        return;
    }
    // SAFETY: checked immediately above.
    let actor_snap = snap.unit(actor).unwrap();

    // Borrow the actor's persistent `AiMemory` directly from the query —
    // writes land in place, no take/put dance. Actors without the component
    // get a short-lived default; mutations to it are discarded when the
    // function returns (matches the previous behaviour, where the write-back
    // branch also silently dropped the memory).
    let mut fallback_memory = AiMemory::default();
    let memory_ref: &mut AiMemory = match memories.get_mut(actor) {
        Ok(m) => m.into_inner(),
        Err(_) => &mut fallback_memory,
    };

    // Capture stored goal state before pre_tick mutates it — used in actor_tick log.
    let memory_pre = memory_ref.last_goal.clone();

    // Step 7.3: centralised goal lifecycle — TTL decay + invalidating clear.
    // Replaces the inline FIXME(step 7) TTL clear on the early-return path.
    goal_lifecycle::pre_tick(memory_ref, &snap, actor_snap);

    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // Step 7.5: write actor_tick log for skip path (no AP/MP).
        if logger.is_enabled() {
            let actor_name = names
                .get(actor)
                .map(|n| n.as_str().to_owned())
                .unwrap_or_else(|_| format!("{:?}", actor));
            let debug_names_skip: HashMap<Entity, String> = snap
                .units
                .iter()
                .map(|u| {
                    let name = names
                        .get(u.entity)
                        .map(|n| n.as_str().to_owned())
                        .unwrap_or_else(|_| format!("{:?}", u.entity));
                    (u.entity, name)
                })
                .collect();
            write_actor_tick_log(logger, ActorTickInput {
                round: combat_ctx.round,
                actor,
                actor_name: &actor_name,
                snapshot: &snap,
                memory_pre: &memory_pre,
                decision: &AiDecision::EndTurn,
                skip_reason: Some("no_ap_no_mp"),
                pool: None,
                intent_reason: None,
                debug_names: &debug_names_skip,
            });
        }
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    // Build influence maps (requires snap, runs only on full path).
    let maps = build_influence_maps(&snap, actor, actor_team, inf_cfg);

    // World-scope context. Per-actor caster/crit-fail-effect/abilities now
    // live on each `UnitSnapshot` row (built by `build_snapshot` above), so
    // there's no parallel `ActorCtx` to thread.
    let crit_fail_chance = 1.0 / settings.crit_fail_die as f32;
    let world = AiWorld {
        content,
        difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance,
        ability_tags,
    };

    // Build name map for debug / log.
    let debug = settings.ai_debug;
    let need_names = debug || logger.is_enabled();
    let debug_names: HashMap<Entity, String> = if need_names {
        snap.units
            .iter()
            .map(|u| {
                let name = names
                    .get(u.entity)
                    .map(|n| n.as_str().to_owned())
                    .unwrap_or_else(|_| format!("{:?}", u.entity));
                (u.entity, name)
            })
            .collect()
    } else {
        HashMap::new()
    };

    // Step 7.4: pick_action is now a pure function (does not mutate memory).
    // update_memory runs AFTER pick_action so that select_intent inside
    // pick_action reads the pre-tick memory state (matching original semantics).
    let result = pick_action(
        actor, actor_pos, &world, &snap, &maps, rng,
        memory_ref, reservations, debug, &debug_names,
    );

    // Update memory with the intent chosen this tick. Must run after pick_action
    // so select_intent inside it saw the pre-tick memory state.
    update_memory(memory_ref, actor_snap, &result.intent, &content.ai_tuning);

    let decision = result.decision.clone();
    let best_idx = result.best_idx;

    // Build ChosenInfo from PickResult for goal_lifecycle::post_tick.
    let fresh_chosen: Option<ChosenInfo> = if result.pool.is_empty() {
        None
    } else {
        let ann = &result.pool.annotations[best_idx];
        let mut chosen_plan = result.pool.plans[best_idx].clone();
        chosen_plan.sim_snapshots.clear();
        Some(ChosenInfo {
            plan: chosen_plan,
            score: ann.score,
            intent: result.intent,
            reason: result.intent_reason.clone(),
        })
    };

    // Step 7.5: write unified actor_tick log (replaces write_decision_log_from_result
    // and the divergence-log block). Divergence data now lives in the continuation
    // section of the actor_tick event.
    if logger.is_enabled() {
        write_actor_tick_log(logger, ActorTickInput {
            round: combat_ctx.round,
            actor,
            actor_name: debug_names.get(&actor).map(|s| s.as_str()).unwrap_or("unknown"),
            snapshot: &snap,
            memory_pre: &memory_pre,
            decision: &decision,
            skip_reason: None,
            pool: Some(&result.pool),
            intent_reason: Some(&result.intent_reason),
            debug_names: &debug_names,
        });
    }

    // Reservations — record committed prefix for this tick.
    if !result.pool.is_empty() {
        let best_plan = &result.pool.plans[best_idx];
        let (_, consumed) = crate::combat::ai::planning::commit_plan(best_plan, actor_pos);
        record_committed_reservations(
            best_plan, consumed, actor_snap, &world, &snap, reservations, actor_pos,
        );
    }

    // Debug overlay — maps + snapshot.
    if debug {
        debug_state.influence_maps = Some(maps.clone());
        if let Some(mut ds) = result.debug_snapshot {
            if debug_state.last_actor == Some(actor) {
                debug_state.plan_index = debug_state.plan_index.saturating_add(1);
            } else {
                debug_state.last_actor = Some(actor);
                debug_state.plan_index = 1;
            }
            ds.plan_index = debug_state.plan_index;
            debug_state.snapshot = Some(ds);
        }
        if matches!(decision, AiDecision::EndTurn) {
            debug_state.last_actor = None;
        }
    }

    // Step 7.3: centralised goal lifecycle post-tick.
    goal_lifecycle::post_tick(
        memory_ref,
        &decision,
        fresh_chosen.as_ref(),
        &snap,
        actor_snap,
        combat_ctx.round,
        world.tuning,
    );

    // Execute decision.
    match decision {
        AiDecision::CastInPlace { ability, target, target_pos } => {
            msgs.use_ability.write(UseAbility { actor, ability, target, target_pos });
        }
        AiDecision::MoveAndCast { path, ability, target, target_pos } => {
            msgs.move_unit.write(MoveUnit { actor, path });
            msgs.use_ability.write(UseAbility { actor, ability, target, target_pos });
        }
        AiDecision::Move { path, .. } => {
            msgs.move_unit.write(MoveUnit { actor, path });
        }
        AiDecision::EndTurn => {
            msgs.end_turn.write(EndTurn { actor });
        }
    }
}

// ── Pact AI: AI controls hero under pact_control status ───────────────────

pub fn has_ai_control_status(entity: Entity, statuses: &Query<&StatusEffects>, content: &ContentView) -> bool {
    statuses.get(entity).is_ok_and(|se| {
        se.0.iter().any(|s| content.statuses.get(&s.id).is_some_and(|d| d.ai_controlled))
    })
}

/// AI for Player heroes under pact_control status. Attacks enemies, heals allies.
pub fn pact_ai_system(
    env: AiEnv,
    mut rng: ResMut<DiceRng>,
    mut reservations: ResMut<Reservations>,
    mut logger: ResMut<AiLogger>,
    mut msgs: AiMessages,
    mut debug_state: ResMut<AiDebugState>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<AiCombatantQ, With<Combatant>>,
    statuses: Query<&StatusEffects>,
    roles: Query<&AxisProfile>,
    mut memories: Query<&mut AiMemory>,
    names: Query<&Name>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Player || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    if !has_ai_control_status(actor, &statuses, &env.content) {
        return;
    }
    run_ai_turn(
        actor, &c, &env, &mut rng, &mut reservations,
        &mut logger, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}

