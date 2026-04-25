#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::repair::{
    classify_continuation_outcome, ContinuationOutcome, ContinuationSeverity,
    FreshDecisionKind, extract_goal_context,
};
use crate::combat::ai::log::AiLogger;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::build_snapshot;
use crate::combat::ai::utility::{pick_action, AiDecision, AiWorld};
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
    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // FIXME(step 7): this path bypasses the divergence log. In the PlanStage
        // pipeline, a start-of-turn stage will write a goal-state event and perform
        // TTL decay centrally. For now — only a cheap proactive stale clear.
        if let Ok(mut mem) = memories.get_mut(actor) {
            if let Some(g) = &mem.last_goal {
                let age = combat_ctx.round.saturating_sub(g.created_round);
                if age >= g.ttl as u32 {
                    mem.last_goal = None;
                }
            }
        }
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    let Some(actor_pos) = positions.get(&actor) else {
        warn!("AI: actor {:?} has no position, ending turn", actor);
        msgs.end_turn.write(EndTurn { actor });
        return;
    };

    // Build snapshot and influence maps.
    let actor_team = c.faction.0;
    let snap = build_snapshot(
        combat_ctx.round, combatants, statuses, positions, roles, content, difficulty,
    );
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

    if snap.unit(actor).is_none() {
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

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

    // Step 6.6: exact-continuation (continuation_from_stored) removed.
    // pick_action applies repair-affinity bonus internally via AiMemory.last_goal,
    // so the fresh plan already reflects goal-preservation preferences.
    let (decision, debug_snapshot, fresh_chosen) = pick_action(
        actor, actor_pos, &world, &snap, &maps, rng,
        memory_ref, reservations, logger, debug, &debug_names,
    );

    // Compute severity from last_goal for divergence logging (step 6.6).
    // None when no stored goal exists — equivalent to old "no mismatch" path.
    let continuation_severity: Option<ContinuationSeverity> = memory_ref.last_goal.as_ref()
        .and_then(|g| {
            let actor_snap = snap.unit(actor).unwrap(); // checked above
            let target_snap = g.target_entity().and_then(|t| snap.unit(t));
            g.check_continuation(actor_snap, target_snap).map(|c| c.severity)
        });

    // Divergence log: emit whenever we have both a stored goal and a fresh plan.
    // The `stored` side is synthesised from last_goal (step 6.6 — StoredPlan removed).
    //
    // `goal_obsolete` is set here (from continuation_outcome) and consumed
    // in the decision-block below to drive proactive stale-goal clearing.
    let mut goal_obsolete = false;
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
        goal_obsolete = matches!(
            continuation_outcome,
            ContinuationOutcome::GoalAbandonedTtlExpired
                | ContinuationOutcome::GoalAbandonedInvalidating
                | ContinuationOutcome::GoalAbandonedVoluntary
                | ContinuationOutcome::GoalAbandonedReactive { .. }
                | ContinuationOutcome::LegacyV25Abandoned { .. }
        );
        let fresh_repair_affinity = Some(fresh.plan.annotation.repair_affinity);
        logger.write_plan_divergence(
            actor,
            stored_goal,
            fresh,
            // used_continuation always false — exact-continuation removed in 6.6.
            false,
            None, // replan_reason: no longer applicable (no stored plan steps to validate)
            continuation_severity,
            continuation_outcome,
            fresh_repair_affinity,
            None, // repair_bonus: not readily available here without re-computing
        );
    }

    // Store debug data: maps always (for overlay), snapshot for console log.
    //
    // `plan_index` counts AI ticks within a single actor's turn. Same actor
    // on the next tick → continuation (re-plan after a Move), so increment.
    // Different actor → new turn elsewhere, reset to 1. EndTurn clears
    // `last_actor` after storing, so the next round this same actor starts
    // at 1 again (without this, a solo AI unit — no other AI actors between
    // its turns to flip `last_actor` — would keep incrementing forever).
    if debug {
        debug_state.influence_maps = Some(maps.clone());
        if let Some(mut ds) = debug_snapshot {
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

    // Step 6.6 / 6.7: manage last_goal lifecycle per decision type.
    // pool_max_score sanity: use chosen.score.max(1.0) as fallback; cancels to
    // confidence=1.0 for the winning plan, which is a safe upper bound.
    match decision {
        AiDecision::Move { ref path, .. } => {
            // Store (or overwrite) goal for the next tick — actor is still en route.
            if let (Some(chosen), Some(dest)) = (fresh_chosen, path.last().copied()) {
                let actor_snap = snap.unit(actor).unwrap();
                let pool_max_score = chosen.score.max(1.0);
                memory_ref.last_goal = extract_goal_context(
                    chosen.intent,
                    &chosen.plan.steps,
                    &chosen.plan.annotation.outcomes,
                    dest,
                    chosen.score,
                    pool_max_score,
                    &snap,
                    actor_snap,
                    combat_ctx.round,
                    world.tuning,
                );
            }
        }
        AiDecision::CastInPlace { .. } | AiDecision::MoveAndCast { .. } => {
            // Climax executed — goal achieved or consumed; clear unconditionally.
            memory_ref.last_goal = None;
        }
        AiDecision::EndTurn => {
            // Preserve the goal across rounds so TTL-decay and cross-round
            // invalidation (target_dead, ttl_expired) become observable.
            // Clear only when continuation_outcome already marks it obsolete.
            if goal_obsolete {
                memory_ref.last_goal = None;
            }
            // Otherwise last_goal survives to the next round; TTL will expire it
            // naturally (via divergence-log classify or early-return clear above).
        }
    }

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
            // No EndTurn here: the next AI tick will continue the stored plan
            // (if freeze is on) or re-plan from scratch (if freeze is off).
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

