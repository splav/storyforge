#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::combat::ai::world::reservations::Reservations;
use combat_engine::modifier;
use crate::combat::DiceRngRes;
use crate::game::components::{CombatStats, Combatant, Initiative, Vital};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatContext, PresetInitiative, TurnQueue};
use bevy::prelude::*;


pub fn build_turn_order(
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut rng: ResMut<DiceRngRes>,
    mut preset: ResMut<PresetInitiative>,
    mut reservations: ResMut<Reservations>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    mut combatants: Query<
        (
            Entity,
            &Name,
            &mut Initiative,
            &CombatStats,
            &Vital,
        ),
        With<Combatant>,
    >,
) {
    ctx.round += 1;
    reservations.clear();

    let first_round = ctx.round == 1;
    let use_preset = first_round && !preset.0.is_empty();

    // (entity, total) for ordering; (entity, dex_mod, roll, total) for logging on round 1.
    let mut init_rolls: Vec<(Entity, i32, i32, i32)> = Vec::new();

    // Include ALL combatants (alive and dead) so that dead units still
    // get a "virtual turn" where their applied statuses tick down.
    let mut order: Vec<(Entity, i32)> = combatants
        .iter_mut()
        .map(|(e, name, mut init, stats, _v)| {
            if first_round {
                if use_preset {
                    if let Some(&saved) = preset.0.get(name.as_str()) {
                        init.0 = saved;
                    } else {
                        let dex_mod = modifier(stats.dexterity);
                        let roll = rng.roll_d(20);
                        init.0 = dex_mod + roll;
                        init_rolls.push((e, dex_mod, roll, init.0));
                    }
                } else {
                    let dex_mod = modifier(stats.dexterity);
                    let roll = rng.roll_d(20);
                    init.0 = dex_mod + roll;
                    init_rolls.push((e, dex_mod, roll, init.0));
                }
            }
            // Reaction refill is owned by the engine (`CombatState::start_round`,
            // invoked at round boundary via `Effect::BumpRound`). The previous
            // ECS-side `r.remaining = r.max` write here was redundant on round
            // 2+ (engine refills internally, projector writes back) and
            // unnecessary on round 1 (CombatantBundle initialises Reactions at
            // max). Deleted in Phase 6 cleanup #4.
            (e, init.0)
        })
        .collect();

    if use_preset {
        preset.0.clear();
    } else if first_round {
        init_rolls.sort_by(|a, b| b.3.cmp(&a.3));
        for (actor, dex_mod, roll, total) in init_rolls {
            log.push(CombatEvent::InitiativeRolled {
                actor,
                dex_mod,
                roll,
                total,
            });
        }
    }

    order.sort_by(|a, b| b.1.cmp(&a.1));

    queue.order = order.into_iter().map(|(e, _)| e).collect();
    // The engine settles from index 0; bootstrap_combat_state calls
    // settle_round_start() which advances past dead/stunned actors.
    // project_state_to_ecs mirrors engine.turn_queue.index back to queue.index
    // so the turn-order UI always shows the correct active slot.
    queue.index = 0;

    // ActiveCombatant is NOT touched here. The engine's TurnEnded/TurnSkipped
    // events (→ remove_active) remove the old holder, and TurnStarted
    // (→ insert_active) sets the new one. apply_bridge_queues_pre_projection
    // always drains both queues. On restart, combatants are despawned entirely
    // so no stale ActiveCombatant component can survive.

    next_phase.set(CombatPhase::AwaitCommand);
}
