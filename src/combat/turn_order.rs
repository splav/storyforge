#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::core::{modifier, DiceRng};
use crate::game::components::{ActionPoints, ActiveCombatant, CombatStats, Combatant, Initiative, Vital};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{CombatContext, PresetInitiative, TurnQueue};
use bevy::prelude::*;

/// Build the turn order for a new round.
/// Initiative is rolled once (round 1) and reused in all subsequent rounds.
pub fn build_turn_order(
    mut commands: Commands,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut rng: ResMut<DiceRng>,
    mut preset: ResMut<PresetInitiative>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut combatants: Query<
        (
            Entity,
            &Name,
            &mut Initiative,
            &mut ActionPoints,
            &CombatStats,
            &Vital,
        ),
        With<Combatant>,
    >,
) {
    ctx.round += 1;
    log.push(CombatEvent::RoundStarted { round: ctx.round });

    let first_round = ctx.round == 1;
    let use_preset = first_round && !preset.0.is_empty();

    // (entity, total) for ordering; (entity, dex_mod, roll, total) for logging on round 1.
    let mut init_rolls: Vec<(Entity, i32, i32, i32)> = Vec::new();

    // Include ALL combatants (alive and dead) so that dead units still
    // get a "virtual turn" where their applied statuses tick down.
    let mut order: Vec<(Entity, i32)> = combatants
        .iter_mut()
        .map(|(e, name, mut init, mut ap, stats, v)| {
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
            if v.is_alive() {
                ap.action = true;
                ap.movement = true;
            }
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
    queue.index = 0;

    for e in &active_q { commands.entity(e).remove::<ActiveCombatant>(); }
    if let Some(first) = queue.current() {
        commands.entity(first).insert(ActiveCombatant);
        log.push(CombatEvent::TurnStarted { actor: first });
    }

    next_phase.set(CombatPhase::AwaitCommand);
}
