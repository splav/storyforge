#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::combat::ai::debug::AiDebugState;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::build_influence_maps;
use crate::combat::ai::role::AiRole;
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
use crate::game::hex::{in_bounds, Hex};
use crate::game::messages::{EndTurn, MoveUnit, UseAbility};
use crate::game::pathfinding::reachable_with_paths;
use crate::game::resources::{CombatContext, GameDb, HexPositions};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// ── Main system ────────────────────────────────────────────────────────────

pub fn enemy_ai_system(
    db: Res<GameDb>,
    settings: Res<GameSettings>,
    difficulty: Res<DifficultyProfile>,
    positions: Res<HexPositions>,
    combat_ctx: Res<CombatContext>,
    mut rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    mut move_unit: MessageWriter<MoveUnit>,
    mut end_turn: MessageWriter<EndTurn>,
    mut debug_state: ResMut<AiDebugState>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<AiCombatantQ, With<Combatant>>,
    statuses: Query<&StatusEffects>,
    roles: Query<&AiRole>,
    names: Query<&Name>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Enemy || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    run_ai_turn(
        actor, Team::Player, &c, &db, &settings, &difficulty, &positions,
        &combat_ctx, &mut rng, &mut use_ability, &mut move_unit, &mut end_turn,
        &combatants, &statuses, &roles, &mut debug_state, &names,
    );
}

/// Shared AI logic for both enemy_ai and pact_ai. `opponent_team` is who to attack.
fn run_ai_turn(
    actor: Entity,
    opponent_team: Team,
    c: &AiCombatantQItem,
    db: &GameDb,
    settings: &GameSettings,
    difficulty: &DifficultyProfile,
    positions: &HexPositions,
    combat_ctx: &CombatContext,
    rng: &mut DiceRng,
    use_ability: &mut MessageWriter<UseAbility>,
    move_unit: &mut MessageWriter<MoveUnit>,
    end_turn: &mut MessageWriter<EndTurn>,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses: &Query<&StatusEffects>,
    roles: &Query<&AiRole>,
    debug_state: &mut AiDebugState,
    names: &Query<&Name>,
) {
    if !c.ap.action && !c.ap.movement {
        end_turn.write(EndTurn { actor });
        return;
    }

    let Some(actor_pos) = positions.get(&actor) else {
        warn!("AI: actor {:?} has no position, ending turn", actor);
        end_turn.write(EndTurn { actor });
        return;
    };

    // Build snapshot and influence maps.
    let actor_team = c.faction.0;
    let snap = build_snapshot(actor, combat_ctx.round, combatants, statuses, positions, roles, db);
    let maps = build_influence_maps(&snap, actor_team, db);

    // Build reachable tiles for movement.
    let own_team_positions: HashSet<Hex> = snap
        .allies_of(actor_team)
        .filter(|u| u.entity != actor)
        .map(|u| u.pos)
        .collect();
    let all_occupied: HashSet<Hex> = positions
        .iter()
        .filter(|(&e, _)| e != actor)
        .map(|(_, &p)| p)
        .collect();
    let reach = reachable_with_paths(
        actor_pos,
        c.speed.0,
        |h| in_bounds(h) && !own_team_positions.contains(&h),
        |h| !all_occupied.contains(&h),
    );

    // Build utility context.
    let caster = build_caster_ctx(c, db);
    let crit_fail_effect = c.combat_path
        .and_then(|cp| db.paths.get(&cp.0))
        .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
    let crit_fail_chance = 1.0 / settings.crit_fail_die as f32;

    let ctx = UtilityContext {
        db,
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

    // Pick action via utility AI.
    let (decision, debug_snapshot) = pick_action(
        actor, actor_pos, &ctx, &snap, &maps, positions, &reach, rng,
        debug, &debug_names,
    );

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
            use_ability.write(UseAbility { actor, ability, target, target_pos });
        }
        AiDecision::MoveAndCast { path, ability, target, target_pos } => {
            move_unit.write(MoveUnit { actor, path });
            use_ability.write(UseAbility { actor, ability, target, target_pos });
        }
        AiDecision::MoveCloser { path } => {
            move_unit.write(MoveUnit { actor, path });
            end_turn.write(EndTurn { actor });
        }
        AiDecision::EndTurn => {
            end_turn.write(EndTurn { actor });
        }
    }
}

// ── Context builders ──────────────────────────────────────────────────────

fn build_caster_ctx(c: &AiCombatantQItem, db: &GameDb) -> CasterContext {
    CasterContext::new(c.stats, Some(c.equipment), &db.weapons)
}

// ── Pact AI: AI controls hero under pact_control status ───────────────────

pub fn has_ai_control_status(entity: Entity, statuses: &Query<&StatusEffects>, db: &GameDb) -> bool {
    statuses.get(entity).is_ok_and(|se| {
        se.0.iter().any(|s| db.statuses.get(&s.id).is_some_and(|d| d.ai_controlled))
    })
}

/// AI for Player heroes under pact_control status. Attacks enemies, heals allies.
pub fn pact_ai_system(
    db: Res<GameDb>,
    settings: Res<GameSettings>,
    difficulty: Res<DifficultyProfile>,
    positions: Res<HexPositions>,
    combat_ctx: Res<CombatContext>,
    mut rng: ResMut<DiceRng>,
    mut use_ability: MessageWriter<UseAbility>,
    mut move_unit: MessageWriter<MoveUnit>,
    mut end_turn: MessageWriter<EndTurn>,
    mut debug_state: ResMut<AiDebugState>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<AiCombatantQ, With<Combatant>>,
    statuses: Query<&StatusEffects>,
    roles: Query<&AiRole>,
    names: Query<&Name>,
) {
    let Ok(actor) = active_q.single() else { return };
    let Ok(c) = combatants.get(actor) else { return };
    if c.faction.0 != Team::Player || !c.vital.is_alive() || c.abilities.0.is_empty() {
        return;
    }
    if !has_ai_control_status(actor, &statuses, &db) {
        return;
    }
    run_ai_turn(
        actor, Team::Enemy, &c, &db, &settings, &difficulty, &positions,
        &combat_ctx, &mut rng, &mut use_ability, &mut move_unit, &mut end_turn,
        &combatants, &statuses, &roles, &mut debug_state, &names,
    );
}
