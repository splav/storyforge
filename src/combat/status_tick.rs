//! DoT-тик статусов на начале хода повесившего.
//!
//! Срабатывает один раз при смене активного бойца (как `turn_start_system`)
//! и тикает все статусы, у которых `applier == active`. Это:
//!
//! * Сохраняет прежнюю семантику «длительность считается в ходах повесившего»
//!   — ход, в котором статус был наложен, не считается: новые статусы
//!   добавляются в `advance_turn_system` в Finalize, уже после тика текущего
//!   хода; первый тик случится только на следующем TurnStart повесившего.
//! * Помещает падение HP в самое начало кадра — `phase_transition_system`
//!   в `Execute` того же кадра успевает оживить фазированного босса до того,
//!   как `advance_turn_system` в Finalize проверит victory-condition.
//!
//! Должен стоять в `TurnStart` **до** `skip_dead` (чтобы явный суицид от
//! собственного тика сразу увели skip'ом в EndTurn) и **до** `apply_auras`
//! (чтобы свежеприменённые аура-статусы с `rounds=1` не тикались в тот же
//! кадр, в котором были выставлены).

use crate::combat::advance_turn::{percent_dot_damage, tick_status_durations, TickResult};
use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{ActiveCombatant, Dead, StatusEffects, Vital};
use bevy::prelude::*;

pub fn tick_status_effects_system(
    mut commands: Commands,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut vitals: Query<&mut Vital>,
    mut statuses: Query<(Entity, &mut StatusEffects)>,
    mut log: ResMut<CombatLog>,
    content: Res<ActiveContent>,
    mut last_ticked: Local<Option<Entity>>,
) {
    let current = active_q.single().ok();
    if current == *last_ticked {
        return;
    }
    *last_ticked = current;
    let Some(actor) = current else { return };

    let tick_results = tick_status_durations(actor, &mut statuses, &content);
    for result in &tick_results {
        match result {
            TickResult::DotDamage { target, damage, status } => {
                if let Ok(mut v) = vitals.get_mut(*target) {
                    v.apply_damage(*damage);
                    log.push(CombatEvent::PoisonTick {
                        target: *target,
                        status: status.clone(),
                        damage: *damage,
                    });
                    if !v.is_alive() {
                        commands.entity(*target).insert(Dead);
                        log.push(CombatEvent::UnitDied { entity: *target });
                    }
                }
            }
            TickResult::PercentDot { target, percent, status } => {
                if let Ok(mut v) = vitals.get_mut(*target) {
                    let damage = percent_dot_damage(v.max_hp, *percent);
                    v.apply_damage(damage);
                    log.push(CombatEvent::PoisonTick {
                        target: *target,
                        status: status.clone(),
                        damage,
                    });
                    if !v.is_alive() {
                        commands.entity(*target).insert(Dead);
                        log.push(CombatEvent::UnitDied { entity: *target });
                    }
                }
            }
            TickResult::Expired { target, status } => {
                log.push(CombatEvent::StatusExpired { target: *target, status: status.clone() });
            }
        }
    }
}
