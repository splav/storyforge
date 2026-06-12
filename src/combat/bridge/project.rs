//! `project_state_to_ecs` — engine state → ECS read-only projection.

use bevy::prelude::*;

use crate::game::components::{
    ActionPoints, BonusMovement, Combatant, Energy, Mana, Rage, Reactions, StatusEffects, Vital,
};
use crate::game::resources::{HexCorpses, HexPositions, TurnQueue};

use super::*;
use combat_engine::PoolKind;

// ── project_state_to_ecs system ──────────────────────────────────────────────

/// Query alias for the ECS components the projector writes.
type ProjectionRow<'a> = (
    &'a mut Vital,
    &'a mut ActionPoints,
    &'a mut Reactions,
    Has<BonusMovement>,
    Option<&'a mut Rage>,
    Option<&'a mut Mana>,
    Option<&'a mut Energy>,
    Option<&'a mut StatusEffects>,
);

/// `Update` system — writes engine `CombatState` back to ECS components.
///
/// Projects:
/// - `pos`              → `HexPositions` (alive) / `HexCorpses` (dead)
/// - `hp`               → `Vital.hp`
/// - `pools[Ap]`        → `ActionPoints.action_points` + `ActionPoints.max_ap`
/// - `pools[Mp]`        → `ActionPoints.movement_points`
/// - `reactions_left`   → `Reactions.remaining`
/// - `reactions_max`    → `Reactions.max`
/// - `pools[Rage]`      → `Rage.current`
/// - `pools[Mana]`      → `Mana.current`
/// - `pools[Energy]`    → `Energy.current`
///
/// Initialise engine `CombatState` from the current ECS snapshot.
///
/// Called on `OnEnter(CombatPhase::AwaitCommand)` once per round (after
/// `build_turn_order` refills AP + reactions into ECS) so the engine has
/// a fresh, authoritative copy of all unit state.
///
/// **5c.1 addition:** also populates the three new per-combat `Unit` fields:
/// - `caster_context` — from `CombatStats` + `Equipment` + optional `CombatPath`
/// - `auras`          — from `AuraSource` ECS component (alive sources only)
/// - `enemy_phases`   — from `EnemyPhases.pending` ECS component
///
/// MP+reactions refill happens in `StartRound` (symmetric with `start_actor_turn`).
///
/// Engine is authoritative for state; ECS is a read-only projection.
///
/// **Layer model:** alive units live in [`HexPositions`] (one-per-hex invariant);
/// dead units live in [`HexCorpses`] (multi-occupant). The two branches below
/// are order-insensitive: whichever entity is iterated first, `remove` on the
/// wrong layer is a no-op, so there is no cross-contamination.
///
/// **C5:** resource values sourced from `Unit.pools[PoolKind::*]` (unified pool
/// table). Legacy fields `unit.mana`, `unit.rage`, `unit.energy`,
/// `unit.action_points`, `unit.movement_points` are no longer read here;
/// they remain write-only (mirrored by C3 mutators) until C6 removes them.
pub fn project_state_to_ecs(
    mut commands: Commands,
    combat_state: Res<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    mut positions: ResMut<HexPositions>,
    mut corpses: ResMut<HexCorpses>,
    mut combatants: Query<ProjectionRow, With<Combatant>>,
    mut queue: ResMut<TurnQueue>,
) {
    for unit in combat_state.0.units() {
        let Some(entity) = id_map.get_entity(unit.id) else {
            // Unit not yet mapped to ECS — skip silently.
            continue;
        };

        if unit.hp() <= 0 {
            // Transition to corpse layer (idempotent — engine.unit.pos is stable).
            positions.remove(&entity);
            corpses.insert(entity, unit.pos);
            // Still sync hp=0 so Vital reflects death; skip AP/MP/Rage/Mana/Energy/Status.
            if let Ok((mut vital, _, _, _, _, _, _, _)) = combatants.get_mut(entity) {
                vital.hp = unit.hp();
            }
            continue;
        }

        // Alive — occupancy layer.
        positions.insert(entity, unit.pos);

        // Write Vital / ActionPoints / Reactions / Rage / Mana / Energy / StatusEffects.
        if let Ok((
            mut vital,
            mut ap,
            mut reactions,
            has_bonus,
            rage_opt,
            mana_opt,
            energy_opt,
            status_effects_opt,
        )) = combatants.get_mut(entity)
        {
            vital.hp = unit.hp();

            // AP / MP — sourced from pools[Ap] / pools[Mp] (C5).
            // Invariant: both are Some for every alive combatant.
            if let Some((ap_cur, ap_max)) = unit.pools[PoolKind::Ap] {
                ap.action_points = ap_cur;
                ap.max_ap = ap_max;
            }
            if let Some((mp_cur, _mp_max)) = unit.pools[PoolKind::Mp] {
                ap.movement_points = mp_cur;
            }

            reactions.remaining = unit.reactions_left as u8;
            reactions.max = unit.reactions_max as u8;

            if has_bonus && ap.movement_points == 0 {
                commands.entity(entity).remove::<BonusMovement>();
            }

            // Project rage.current when both sides carry a rage pool.
            if let (Some((engine_current, _engine_max)), Some(mut ecs_rage)) =
                (unit.pools[PoolKind::Rage], rage_opt)
            {
                ecs_rage.current = engine_current;
            }

            // Project mana.current when both sides carry a mana pool.
            if let (Some((current, _max)), Some(mut mana_comp)) =
                (unit.pools[PoolKind::Mana], mana_opt)
            {
                mana_comp.current = current;
            }

            // Project energy.current when both sides carry an energy pool.
            if let (Some((current, _max)), Some(mut energy_comp)) =
                (unit.pools[PoolKind::Energy], energy_opt)
            {
                energy_comp.current = current;
            }

            // Merge statuses: preserve ECS entries the engine doesn't know about.
            if let Some(mut status_effects) = status_effects_opt {
                let engine_known: std::collections::HashSet<(
                    &combat_engine::StatusId,
                    combat_engine::state::EffectSource,
                )> = unit.statuses.iter().map(|s| (&s.id, s.applier)).collect();

                // Env-applied engine statuses project to ECS with `applier: None`,
                // losing their `Env(id)` identity. Track their status *ids* so the
                // preserve filter can recognise the engine's own env statuses on the
                // ECS side. Without this, an env status (e.g. a spike-trap
                // `disoriented`) is both preserved AND re-appended from the engine
                // every frame → the list grows by one per frame and the status never
                // expires. The engine dedupes by id, so id-only matching is exact.
                let engine_env_ids: std::collections::HashSet<&combat_engine::StatusId> = unit
                    .statuses
                    .iter()
                    .filter(|s| matches!(s.applier, combat_engine::state::EffectSource::Env(_)))
                    .map(|s| &s.id)
                    .collect();

                // Preserve ECS statuses that are NOT in the engine's status list.
                // For ECS statuses with a unit applier we key on
                // `EffectSource::Unit(entity_to_uid(applier_entity))`.
                let preserved: Vec<crate::game::components::ActiveStatus> = status_effects
                    .0
                    .iter()
                    .filter(|ecs_s| {
                        match ecs_s.applier {
                            Some(applier_ent) => !engine_known.contains(&(
                                &ecs_s.id,
                                combat_engine::state::EffectSource::Unit(entity_to_uid(
                                    applier_ent,
                                )),
                            )),
                            // applier: None means env-applied. Preserve it ONLY if the
                            // engine has no matching env status — otherwise the engine
                            // re-projects it below (single source of truth), and keeping
                            // the ECS copy too would double it every frame.
                            None => !engine_env_ids.contains(&ecs_s.id),
                        }
                    })
                    .cloned()
                    .collect();

                let mut new_list: Vec<crate::game::components::ActiveStatus> = preserved;
                for engine_s in &unit.statuses {
                    // R2: map engine EffectSource back to an optional ECS Entity.
                    // EffectSource::Unit → Some(entity); EffectSource::Env → None
                    // (no unit entity represents an environment applier).
                    let applier_opt: Option<Entity> = match engine_s.applier {
                        combat_engine::state::EffectSource::Unit(uid) => {
                            Some(id_map.get_entity(uid).unwrap_or(entity))
                        }
                        combat_engine::state::EffectSource::Env(_) => None,
                    };
                    new_list.push(crate::game::components::ActiveStatus {
                        id: engine_s.id.clone(),
                        rounds_remaining: engine_s.rounds_remaining,
                        dot_per_tick: engine_s.dot_per_tick,
                        applier: applier_opt,
                    });
                }

                status_effects.0 = new_list;
            }
        }
    }

    // ── Project engine turn order + index → ECS TurnQueue ────────────────────
    // The engine owns the authoritative turn order after round-1 bootstrap.
    // On round-2+ Execute frames this keeps the UI strip in sync with the
    // engine's current cursor (turn_queue.index may advance as turns end).
    if !combat_state.0.units().is_empty() {
        queue.order = combat_state
            .0
            .turn_queue
            .order
            .iter()
            .filter_map(|uid| id_map.get_entity(*uid))
            .collect();
        queue.index = combat_state.0.turn_queue.index;
    }
}
