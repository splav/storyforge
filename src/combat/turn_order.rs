use bevy::prelude::*;
use crate::app_state::CombatPhase;
use crate::core::DiceRng;
use crate::game::components::{ActionPoints, Combatant, CombatStats, Initiative, Vital};
use crate::game::resources::{CombatContext, CombatEvent, CombatLog, TurnQueue};

/// Build the turn order for a new round.
/// Initiative is rolled once (round 1) and reused in all subsequent rounds.
pub fn build_turn_order(
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut rng: ResMut<DiceRng>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    mut combatants: Query<
        (Entity, &mut Initiative, &mut ActionPoints, &CombatStats, &Vital),
        With<Combatant>,
    >,
) {
    ctx.round += 1;
    log.push(CombatEvent::RoundStarted { round: ctx.round });

    let first_round = ctx.round == 1;

    let mut order: Vec<(Entity, i32)> = combatants
        .iter_mut()
        .filter(|(_, _, _, _, v)| v.is_alive())
        .map(|(e, mut init, mut ap, stats, _)| {
            if first_round {
                init.0 = stats.initiative + rng.roll_d(20);
            }
            ap.action = true;
            (e, init.0)
        })
        .collect();

    order.sort_by(|a, b| b.1.cmp(&a.1));
    queue.order = order.into_iter().map(|(e, _)| e).collect();
    queue.index = 0;

    if let Some(first) = queue.current() {
        ctx.active = Some(first);
        log.push(CombatEvent::TurnStarted { actor: first });
    }

    next_phase.set(CombatPhase::AwaitCommand);
}
