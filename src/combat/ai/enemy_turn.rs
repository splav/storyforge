#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::content::content_view::{ActiveContent, ContentView};
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::build_snapshot;
use crate::combat::ai::utility::{pick_action, AiDecision, UtilityContext};
use crate::content::abilities::CasterContext;
use crate::content::races::CritFailEffect;
use crate::content::settings::GameSettings;
use crate::core::DiceRng;
use crate::game::components::{
    ActiveCombatant, AiCombatantQ, AiCombatantQItem,
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

// ── Main system ────────────────────────────────────────────────────────────

pub fn enemy_ai_system(
    content: Res<ActiveContent>,
    settings: Res<GameSettings>,
    difficulty: Res<DifficultyProfile>,
    inf_cfg: Res<InfluenceConfig>,
    positions: Res<HexPositions>,
    combat_ctx: Res<CombatContext>,
    mut rng: ResMut<DiceRng>,
    mut reservations: ResMut<Reservations>,
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
        actor, Team::Player, &c, &content, &settings, &difficulty, &inf_cfg, &positions,
        &combat_ctx, &mut rng, &mut reservations, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}

/// Shared AI logic for both enemy_ai and pact_ai. `opponent_team` is who to attack.
fn run_ai_turn(
    actor: Entity,
    opponent_team: Team,
    c: &AiCombatantQItem,
    content: &ContentView,
    settings: &GameSettings,
    difficulty: &DifficultyProfile,
    inf_cfg: &InfluenceConfig,
    positions: &HexPositions,
    combat_ctx: &CombatContext,
    rng: &mut DiceRng,
    reservations: &mut Reservations,
    msgs: &mut AiMessages,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses: &Query<&StatusEffects>,
    roles: &Query<&AxisProfile>,
    memories: &mut Query<&mut AiMemory>,
    debug_state: &mut AiDebugState,
    names: &Query<&Name>,
) {
    if !c.ap.action && !c.ap.movement {
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
    };

    // Build name map for debug.
    let debug = settings.ai_debug;
    let debug_names: HashMap<Entity, String> = if debug {
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

    // Pick action via utility AI.
    let (decision, debug_snapshot) = pick_action(
        actor, actor_pos, &ctx, &snap, &maps, &reach, rng,
        &mut memory, reservations, debug, &debug_names,
    );

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
            msgs.move_unit.write(MoveUnit { actor, path });
            msgs.end_turn.write(EndTurn { actor });
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

// ── Pact AI: AI controls hero under pact_control status ───────────────────

pub fn has_ai_control_status(entity: Entity, statuses: &Query<&StatusEffects>, content: &ContentView) -> bool {
    statuses.get(entity).is_ok_and(|se| {
        se.0.iter().any(|s| content.statuses.get(&s.id).is_some_and(|d| d.ai_controlled))
    })
}

/// AI for Player heroes under pact_control status. Attacks enemies, heals allies.
pub fn pact_ai_system(
    content: Res<ActiveContent>,
    settings: Res<GameSettings>,
    difficulty: Res<DifficultyProfile>,
    inf_cfg: Res<InfluenceConfig>,
    positions: Res<HexPositions>,
    combat_ctx: Res<CombatContext>,
    mut rng: ResMut<DiceRng>,
    mut reservations: ResMut<Reservations>,
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
    if !has_ai_control_status(actor, &statuses, &content) {
        return;
    }
    run_ai_turn(
        actor, Team::Enemy, &c, &content, &settings, &difficulty, &inf_cfg, &positions,
        &combat_ctx, &mut rng, &mut reservations, &mut msgs,
        &combatants, &statuses, &roles, &mut memories, &mut debug_state, &names,
    );
}
