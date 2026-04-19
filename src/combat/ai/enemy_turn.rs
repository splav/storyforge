#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::log::AiLogger;
use crate::combat::ai::planning::{
    decision_from_steps, steps_consumed_by_decision, validate_plan_step,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::build_snapshot;
use crate::combat::ai::utility::{pick_action, AiDecision, UtilityContext};
use crate::content::abilities::CasterContext;
use crate::content::races::CritFailEffect;
use crate::content::settings::GameSettings;
use crate::core::DiceRng;
use crate::game::components::{
    ActivePlan, ActivePlans, ActiveCombatant, AiCombatantQ, AiCombatantQItem,
    Combatant, StatusEffects, Team,
};
use crate::game::hex::{can_stop_on, is_passable, Hex};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{CombatContext, HexPositions};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// ── Bundled message writers (keeps system params under Bevy's 16-param limit) ──

#[derive(SystemParam)]
pub struct AiMessages<'w> {
    use_ability: MessageWriter<'w, UseAbility>,
    move_unit: MessageWriter<'w, MoveUnit>,
    end_turn: MessageWriter<'w, EndTurn>,
}

/// Shared read-only resources used during AI decision making. Bundling
/// everything we just *read* into one `SystemParam` slot keeps the two AI
/// systems under Bevy's 16-parameter limit while we add mutable state like
/// `ActivePlans` and `Reservations`.
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
    mut active_plans: ResMut<ActivePlans>,
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
        actor, Team::Player, &c, &env, &mut rng, &mut reservations, &mut active_plans,
        &mut logger, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}

/// Shared AI logic for both enemy_ai and pact_ai. `opponent_team` is who to attack.
fn run_ai_turn(
    actor: Entity,
    opponent_team: Team,
    c: &AiCombatantQItem,
    env: &AiEnv,
    rng: &mut DiceRng,
    reservations: &mut Reservations,
    active_plans: &mut ActivePlans,
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
    let snap = build_snapshot(actor, combat_ctx.round, combatants, statuses, positions, roles, content);
    let maps = build_influence_maps(&snap, actor_team, inf_cfg);

    // Build reachable tiles for movement. Use the snapshot's status-adjusted
    // speed so paths respect debuffs like Истощение — otherwise the AI plans
    // routes that movement_system silently rejects, dropping the action.
    // .max(0) lets `speed = 0` (immobile) stay zero, while still clamping negative
    // status debuffs. With effective_speed = 0, reachable_with_paths returns only
    // the actor's own tile, so AI naturally plans without movement.
    let effective_speed = snap
        .unit(actor)
        .map(|u| u.speed.max(0))
        .unwrap_or(c.speed.0);
    // Passability matches player movement rules: enemies block, allies are walked
    // through. `can_stop` still uses all occupied tiles — a unit can't end its
    // move on top of a teammate even though it can pass through.
    let enemy_positions: HashSet<Hex> = snap
        .enemies_of(actor_team)
        .map(|u| u.pos)
        .collect();
    let all_occupied: HashSet<Hex> = positions
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();
    let reach = reachable_with_paths(
        actor_pos,
        effective_speed,
        |h| is_passable(h, &enemy_positions),
        |h| can_stop_on(h, &all_occupied, None),
    );

    // Build utility context.
    let caster = build_caster_ctx(c, content);
    let crit_fail_effect = c.combat_path
        .and_then(|cp| content.paths.get(&cp.0))
        .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
    let crit_fail_chance = 1.0 / settings.crit_fail_die as f32;

    let ctx = UtilityContext {
        content,
        difficulty,
        caster: &caster,
        abilities: c.abilities,
        opponent_team,
        crit_fail_effect,
        crit_fail_chance,
        blocked_tiles: &all_occupied,
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

    // Get or create AI memory for this actor.
    let mut memory = memories
        .get_mut(actor)
        .map(|mut m| std::mem::take(&mut *m))
        .unwrap_or_default();

    // ── Resume a committed plan if one exists and still validates ──────
    // If the stored plan's next step still executes cleanly in the current
    // world, commit it (bundling Move→Cast as usual) and skip fresh pick_action
    // entirely — this is what gives multi-step plans their *coherence* across
    // ticks (the retreat lands on the exact planned tile).
    let Some(active_snap) = snap.unit(actor) else {
        msgs.end_turn.write(EndTurn { actor });
        return;
    };
    let resumed: Option<AiDecision> = try_resume_plan(
        actor, actor_pos, &ctx, &snap, active_snap, active_plans,
    );

    let (decision, debug_snapshot) = if let Some(d) = resumed {
        (d, None)
    } else {
        // Drop any stale plan so store-below doesn't overwrite a wrong one.
        active_plans.0.remove(&actor);
        let (d, ds, plan) = pick_action(
            actor, actor_pos, &ctx, &snap, &maps, &reach, rng,
            &mut memory, reservations, logger, debug, &debug_names,
        );
        // Store plan iff it has steps beyond what the first commit consumes.
        if let Some(plan) = plan {
            let consumed = steps_consumed_by_decision(&plan.steps);
            if consumed < plan.steps.len() {
                active_plans.0.insert(
                    actor,
                    ActivePlan { steps: plan.steps, cursor: consumed },
                );
            }
        }
        (d, ds)
    };

    // Write memory back.
    if let Ok(mut mem) = memories.get_mut(actor) {
        *mem = memory;
    }

    // Store debug data: maps always (for overlay), snapshot for console log.
    if debug {
        debug_state.influence_maps = Some(maps.clone());
        if let Some(ds) = debug_snapshot {
            debug_state.snapshot = Some(ds);
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
        AiDecision::MoveCloser { path } | AiDecision::MoveOnlyRetreat { path } => {
            // No EndTurn here: the next AI tick will re-plan with the updated
            // pool. If nothing useful remains, `fallback_move`/the guard at the
            // top of `run_ai_turn` will emit EndTurn.
            msgs.move_unit.write(MoveUnit { actor, path });
        }
        AiDecision::EndTurn => {
            msgs.end_turn.write(EndTurn { actor });
        }
    }
}

// ── Context builders ──────────────────────────────────────────────────────

fn build_caster_ctx(c: &AiCombatantQItem, content: &ContentView) -> CasterContext {
    CasterContext::new(c.stats, Some(c.equipment), &content.weapons)
}

/// Short human-readable representation of a plan step for debug logs.
fn fmt_step(step: &crate::combat::ai::planning::PlanStep) -> String {
    use crate::combat::ai::planning::PlanStep;
    match step {
        PlanStep::Move { path } => {
            let last = path.last().copied().unwrap_or_default();
            format!("Move→({},{}) via {} tiles", last.x, last.y, path.len())
        }
        PlanStep::Cast { ability, target, target_pos } => format!(
            "Cast {} → {:?} @ ({},{})",
            ability.0, target, target_pos.x, target_pos.y,
        ),
    }
}

/// Try to resume the actor's stored plan: validate the current cursor step,
/// and — if valid — commit it (bundling Move→Cast when applicable) and advance
/// the cursor. Returns `Some(decision)` on successful resume; returns `None`
/// (and does **not** mutate the stored plan) if no plan exists or validation
/// fails. Caller is responsible for dropping a stale plan before replanning.
fn try_resume_plan(
    actor: Entity,
    actor_pos: crate::game::hex::Hex,
    ctx: &UtilityContext,
    snap: &crate::combat::ai::snapshot::BattleSnapshot,
    active_snap: &crate::combat::ai::snapshot::UnitSnapshot,
    active_plans: &mut ActivePlans,
) -> Option<AiDecision> {
    let plan = active_plans.0.get_mut(&actor)?;
    if plan.cursor >= plan.steps.len() {
        active_plans.0.remove(&actor);
        return None;
    }

    let suffix = &plan.steps[plan.cursor..];
    // Validate the *first* step of the suffix. Bundled Move→Cast validates
    // the move here; the cast half will validate on its own next frame if
    // we somehow split the bundle — current engine commits both atomically
    // so a single validation is sufficient.
    let next_step = &suffix[0];
    if let Err(reason) = validate_plan_step(next_step, active_snap, snap, ctx) {
        info!(
            "AI plan invalidated for {:?} at cursor {}/{}: {} ({})",
            actor,
            plan.cursor,
            plan.steps.len(),
            reason,
            fmt_step(next_step),
        );
        return None;
    }
    // If the bundle is Move→Cast, also validate the cast against the
    // post-move position to catch "cast from new tile fails" cases.
    if suffix.len() >= 2 {
        if let (
            crate::combat::ai::planning::PlanStep::Move { path },
            crate::combat::ai::planning::PlanStep::Cast { .. },
        ) = (&suffix[0], &suffix[1])
        {
            if let Some(&dest) = path.last() {
                // Synthesize a projected actor at the move destination — only
                // `pos`, `action_points`, resources matter for the cast validation.
                let mut projected = active_snap.clone();
                projected.pos = dest;
                projected.movement_points =
                    (projected.movement_points - path.len() as i32).max(0);
                if let Err(reason) = validate_plan_step(&suffix[1], &projected, snap, ctx) {
                    info!(
                        "AI plan bundle cast invalid for {:?} ({}): {}",
                        actor,
                        fmt_step(&suffix[1]),
                        reason,
                    );
                    return None;
                }
            }
        }
    }

    let decision = decision_from_steps(suffix, actor, actor_pos);
    let consumed = steps_consumed_by_decision(suffix);
    plan.cursor += consumed;
    if plan.cursor >= plan.steps.len() {
        active_plans.0.remove(&actor);
    }
    Some(decision)
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
    mut active_plans: ResMut<ActivePlans>,
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
        actor, Team::Enemy, &c, &env, &mut rng, &mut reservations, &mut active_plans,
        &mut logger, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}
