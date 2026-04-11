use crate::app_state::CombatPhase;
use crate::core::{modifier, DiceRng};
use crate::game::components::{ActionPoints, CombatStats, Combatant, Initiative, Vital};
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, TurnQueue};
use bevy::prelude::*;

/// Build the turn order for a new round.
/// Initiative is rolled once (round 1) and reused in all subsequent rounds.
pub fn build_turn_order(
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut rng: ResMut<DiceRng>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
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

    // (entity, total) for ordering; (entity, dex_mod, roll, total) for logging on round 1.
    let mut init_rolls: Vec<(Entity, i32, i32, i32)> = Vec::new();

    let mut order: Vec<(Entity, i32)> = combatants
        .iter_mut()
        .filter(|(_, _, _, _, _, v)| v.is_alive())
        .map(|(e, _, mut init, mut ap, stats, _)| {
            if first_round {
                let dex_mod = modifier(stats.dexterity);
                let roll = rng.roll_d(20);
                init.0 = dex_mod + roll;
                init_rolls.push((e, dex_mod, roll, init.0));
            }
            ap.action = true;
            (e, init.0)
        })
        .collect();

    if first_round {
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

    if let Some(first) = queue.current() {
        ctx.active = Some(first);
        log.push(CombatEvent::TurnStarted { actor: first });
    }

    next_phase.set(CombatPhase::AwaitCommand);
}
