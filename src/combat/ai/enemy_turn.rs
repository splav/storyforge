#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::repair::{
    classify_continuation_outcome, ContinuationSeverity,
    FreshDecisionKind,
};
use crate::combat::ai::repair::lifecycle as goal_lifecycle;
use crate::combat::ai::log::AiLogger;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::build_snapshot;
use crate::combat::ai::intent::update_memory;
use crate::combat::ai::planning::record_committed_reservations;
use crate::combat::ai::utility::{
    pick_action, write_decision_log_from_result, AiDecision, AiWorld, ChosenInfo,
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

    // Step 7.3: centralised goal lifecycle — TTL decay + invalidating clear.
    // Replaces the inline FIXME(step 7) TTL clear on the early-return path.
    goal_lifecycle::pre_tick(memory_ref, &snap, actor_snap);

    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // tick_skipped log will be written here in 7.5; for now just end turn.
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    // Build influence maps (requires snap, runs only on full path).
    let maps = build_influence_maps(&snap, actor, actor_team, inf_cfg);

    // World-scope context. Per-actor caster/crit-fail-effect/abilities now
    // live on each `UnitSnapshot` row (built by `build_snapshot` above), so
    // there's no parallel `ActorCtx` to thread.
    let crit_fail_chance = 1.0 / settings.crit_fail_die as f32;
    let world = AiWorld { content, difficulty, tuning: &content.ai_tuning, crit_fail_chance };

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
    let t0 = if logger.is_enabled() { Some(std::time::Instant::now()) } else { None };
    let result = pick_action(
        actor, actor_pos, &world, &snap, &maps, rng,
        memory_ref, reservations, debug, &debug_names,
    );

    // Update memory with the intent chosen this tick. Must run after pick_action
    // so select_intent inside it saw the pre-tick memory state.
    update_memory(memory_ref, actor_snap, &result.intent, &content.ai_tuning);

    let decision = result.decision.clone();
    let best_idx = result.best_idx;

    // Build ChosenInfo from PickResult for divergence log + goal_lifecycle.
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

    // Logging — orchestrator receives PickResult and writes the decision log.
    if logger.is_enabled() {
        let decision_time_ms = t0.map_or(0, |t| t.elapsed().as_millis() as u64);
        let reservations_snap = reservations.to_snapshot();
        write_decision_log_from_result(
            logger, decision_time_ms, actor, actor_snap, &snap, content,
            &result, &debug_names, &env.difficulty, memory_ref, reservations_snap,
        );
    }

    // Reservations — record committed prefix for this tick.
    if !result.pool.is_empty() {
        let best_plan = &result.pool.plans[best_idx];
        let (_, consumed) = crate::combat::ai::planning::commit_plan(best_plan, actor_pos);
        record_committed_reservations(
            best_plan, consumed, actor_snap, &world, &snap, reservations, actor_pos,
        );
    }

    // Compute severity from last_goal for divergence logging (step 6.6).
    // None when no stored goal exists — equivalent to old "no mismatch" path.
    let continuation_severity: Option<ContinuationSeverity> = memory_ref.last_goal.as_ref()
        .and_then(|g| {
            let actor_snap = snap.unit(actor).unwrap(); // checked above
            let target_snap = g.target_entity().and_then(|t| snap.unit(t));
            g.check_continuation(actor_snap, target_snap).map(|c| c.severity)
        });

    // Divergence log block (step 6.6). Remains untouched until 7.5.
    if let (Some(ref stored_goal), Some(ref fresh)) = (&memory_ref.last_goal, &fresh_chosen) {
        let fresh_decision_kind = match decision {
            AiDecision::CastInPlace { .. } | AiDecision::MoveAndCast { .. } => {
                FreshDecisionKind::Cast
            }
            AiDecision::Move { .. } => FreshDecisionKind::Move,
            AiDecision::EndTurn => FreshDecisionKind::EndTurn,
        };
        let fresh_reason = &fresh.reason;
        let age = combat_ctx.round.saturating_sub(stored_goal.created_round);
        let continuation_outcome = classify_continuation_outcome(
            Some(stored_goal),
            fresh.intent,
            fresh_decision_kind,
            fresh_reason,
            continuation_severity,
            age,
        );
        let fresh_repair_affinity = Some(fresh.plan.annotation.repair_affinity);
        logger.write_plan_divergence(
            actor,
            stored_goal,
            fresh,
            false,
            None,
            continuation_severity,
            continuation_outcome,
            fresh_repair_affinity,
            None,
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

