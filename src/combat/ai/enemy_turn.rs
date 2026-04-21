#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::{AiMemory, PlanSnapshot, StoredPlan};
use crate::combat::ai::planning::types::PlanStep;
use crate::combat::ai::snapshot::BattleSnapshot;
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
    let world = AiWorld { content, difficulty, crit_fail_chance };

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

    // Always build the fresh plan — needed for (a) normal use when no stored
    // plan exists, (b) shadow comparison for divergence diagnostics, and (c)
    // fallback when the stored plan is invalidated.
    let (fresh_decision, debug_snapshot, fresh_chosen) = pick_action(
        actor, actor_pos, &world, &snap, &maps, rng,
        memory_ref, reservations, logger, debug, &debug_names,
    );

    // Take the stored plan out of memory (clears it unconditionally; we
    // re-insert it below only when emitting another Move this tick).
    let old_plan = memory_ref.last_plan.take();

    // Freeze: if the previous tick was a MoveOnly and the plan is still valid,
    // continue from step[step_index] instead of following the fresh plan.
    // This suppresses "forward-then-backward" oscillation caused by a scorer
    // that is non-monotonic under partial plan execution.
    //
    // `used_continuation` and `replan_reason` are tracked for divergence logs.
    let mut used_continuation = false;
    let mut replan_reason: Option<&'static str> = None;

    let decision = if settings.ai_freeze_plan_after_move {
        if let Some(ref stored) = old_plan {
            let actor_snap = snap.unit(actor).unwrap(); // checked above
            let target_snap = stored.snapshot.target.and_then(|t| snap.unit(t));
            let mismatch = stored.snapshot.mismatch(actor_snap, target_snap);
            if let Some(reason) = mismatch {
                // State changed (AoO, status, target moved/dead) — replan.
                replan_reason = Some(reason);
                fresh_decision
            } else {
                // Snapshot is valid — try to continue the stored plan.
                match continuation_from_stored(stored, actor_snap, &snap, content) {
                    Some(cont) => {
                        used_continuation = true;
                        cont
                    }
                    None => {
                        replan_reason = Some("continuation_invalid");
                        fresh_decision
                    }
                }
            }
        } else {
            fresh_decision
        }
    } else {
        fresh_decision
    };

    // Divergence log: emit whenever we had both a stored plan and a fresh plan.
    if let (Some(ref stored), Some(ref fresh)) = (&old_plan, &fresh_chosen) {
        logger.write_plan_divergence(actor, stored, fresh, used_continuation, replan_reason);
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

    // After a Move decision, store the plan so the next tick can continue it.
    // For all other decisions the plan is already cleared (last_plan was taken).
    if let AiDecision::Move { ref path, .. } = decision {
        if let (Some(chosen), Some(dest)) = (fresh_chosen, path.last().copied()) {
            let actor_snap = snap.unit(actor).unwrap();
            let intent_target = chosen.intent.target().and_then(|t| snap.unit(t));
            let snapshot = PlanSnapshot::capture(actor_snap, intent_target, dest);
            let (cast_ability, cast_target) = match chosen.plan.steps.get(1) {
                Some(PlanStep::Cast { ability, target, .. }) => {
                    (Some(ability.clone()), Some(*target))
                }
                _ => (None, None),
            };
            memory_ref.last_plan = Some(StoredPlan {
                steps: chosen.plan.steps,
                step_index: 1,
                snapshot,
                intent: chosen.intent.kind(),
                cast_ability,
                cast_target,
                score: chosen.score,
            });
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

// ── Plan continuation ──────────────────────────────────────────────────────

/// Attempt to produce a decision by executing step[`stored.step_index`] from
/// the stored plan. Returns `None` when:
/// - There is no step at that index (plan exhausted → EndTurn is left to the
///   AP-empty guard at the top of `run_ai_turn`).
/// - The next step is a Move (multi-move plans aren't continued — rare and
///   handled cleanly by falling back to the fresh plan).
/// - The Cast step fails validation (target dead, out of range, no resources).
fn continuation_from_stored(
    stored: &StoredPlan,
    actor: &crate::combat::ai::snapshot::UnitSnapshot,
    snap: &BattleSnapshot,
    content: &crate::content::content_view::ContentView,
) -> Option<AiDecision> {
    use crate::core::ResourceKind;

    let next = stored.steps.get(stored.step_index)?;
    let PlanStep::Cast { ability, target, target_pos } = next else {
        return None; // Move steps: fall back to fresh plan
    };

    let def = content.abilities.get(ability)?;

    // Target must still be alive in the snapshot.
    let target_snap = snap.unit(*target)?;

    // Range check (same logic as validate_action_system).
    if def.range.max > 0 {
        let dist = actor.pos.unsigned_distance_to(target_snap.pos);
        if dist > def.range.max {
            return None;
        }
    }

    // AP check.
    if actor.action_points < def.cost_ap {
        return None;
    }

    // Resource checks.
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Mana   => actor.mana.map(|(v, _)| v).unwrap_or(0),
            ResourceKind::Rage   => actor.rage.map(|(v, _)| v).unwrap_or(0),
            ResourceKind::Energy => actor.energy.map(|(v, _)| v).unwrap_or(0),
            ResourceKind::Hp     => actor.hp,
        };
        if available < cost.amount {
            return None;
        }
    }

    Some(AiDecision::CastInPlace {
        ability: ability.clone(),
        target: *target,
        target_pos: *target_pos,
    })
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::intent::{IntentKind, PlanSnapshot, StoredPlan};
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::content_view::ContentView;
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn ent(id: u32) -> Entity { Entity::from_raw_u32(id).expect("valid") }

    /// Minimal `StoredPlan` with a Cast at step index 1.
    fn stored_with_cast(ability: &str, target: Entity, target_pos: crate::game::hex::Hex) -> StoredPlan {
        let dummy_pos = hex_from_offset(0, 0);
        StoredPlan {
            steps: vec![
                PlanStep::Move { path: vec![dummy_pos] },
                PlanStep::Cast { ability: AbilityId::from(ability), target, target_pos },
            ],
            step_index: 1,
            snapshot: PlanSnapshot::capture(
                &UnitBuilder::new(1, Team::Enemy, dummy_pos).build(),
                None,
                dummy_pos,
            ),
            intent: IntentKind::FocusTarget,
            cast_ability: Some(AbilityId::from(ability)),
            cast_target: Some(target),
            score: 1.0,
        }
    }

    #[test]
    fn continuation_succeeds_valid_melee_cast() {
        let content = ContentView::load_global_for_tests();
        let actor_pos = hex_from_offset(1, 0);
        let target_pos = hex_from_offset(2, 0); // adjacent — dist=1, melee range=1
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).ap(1).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let stored = stored_with_cast("melee_attack", ent(2), target_pos);

        let result = continuation_from_stored(&stored, &actor, &snap, &content);
        assert!(
            matches!(result, Some(AiDecision::CastInPlace { .. })),
            "valid adjacent cast should produce CastInPlace",
        );
    }

    #[test]
    fn continuation_rejects_out_of_range_target() {
        let content = ContentView::load_global_for_tests();
        let actor_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(5, 0); // dist=5 > melee range=1
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).ap(1).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let stored = stored_with_cast("melee_attack", ent(2), target_pos);

        assert!(continuation_from_stored(&stored, &actor, &snap, &content).is_none());
    }

    #[test]
    fn continuation_rejects_zero_ap() {
        let content = ContentView::load_global_for_tests();
        let actor_pos = hex_from_offset(1, 0);
        let target_pos = hex_from_offset(2, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).ap(0).build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let stored = stored_with_cast("melee_attack", ent(2), target_pos);

        assert!(continuation_from_stored(&stored, &actor, &snap, &content).is_none());
    }

    #[test]
    fn continuation_rejects_dead_target() {
        let content = ContentView::load_global_for_tests();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(1, 0)).ap(1).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1); // target not in snap
        let stored = stored_with_cast("melee_attack", ent(2), hex_from_offset(2, 0));

        assert!(continuation_from_stored(&stored, &actor, &snap, &content).is_none());
    }
}
