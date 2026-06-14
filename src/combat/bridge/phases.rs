//! Boss-phase ECS writes and phase-override application.

use bevy::prelude::*;

use crate::combat::ai::config::role::{infer_profile, AxisProfile};
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::content::content_view::ActiveContent;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{
    Abilities, AiBehaviorOverride, CombatStats, Dead, EnemyPhases, Speed, Tags, VictoryTarget,
    Vital,
};
use crate::game::resources::{
    CombatContext, CombatObjective, PhaseDeadline, PhaseDeadlineState, UiDirty, UiDirtyFlags,
};

use super::*;
use combat_engine::state::UnitId;

// ── apply_phase_ecs_writes ────────────────────────────────────────────────────

/// Apply ECS-only deltas for a boss phase transition.
///
/// Called for each `Event::PhaseEntered` seen in a translator event stream.
/// Reproduces the logic of the deleted `phase_transition_system` (4d/4e):
///   1. Reads `EnemyPhases.pending[phase_idx]` for the new Name, Abilities,
///      CombatStats, and flavor text.
///   2. Mutates ECS components: `Name`, `Abilities`, `CombatStats`, `Vital`,
///      `Speed` (re-infers `AxisProfile`; removes `Dead` if `heal_to_full` revived).
///   3. Mirrors `engine_unit.runtime` → `Vital.armor`/`magic_resist` and `Speed`
///      so non-engine consumers (UI, legality, AxisProfile) see the new values.
///      The engine already applied `EnterPhase` before this runs; reading
///      `unit.runtime` here is the single derivation (no recompute).
///   4. Pops `pending[phase_idx]` (spec §8: exactly one pop per event).
///   5. Pushes `CombatEvent::PhaseEntered` with `prev_name`/`next_name`/`flavor`.
///   6. If the phase carries `victory_override` or `turn_limit`, pushes a
///      `PhaseOverrideIntent` into `overrides` for deferred application.
///
/// Called from `apply_bridge_queues_post_projection` which runs AFTER `project_state_to_ecs`
/// to avoid a query conflict over `&mut Vital` between the two systems.
/// `process_action_system` and `bootstrap_combat_state` record `(unit, phase_idx)`
/// pairs into `PendingPhaseTransitions`; this helper drains them.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn apply_phase_ecs_writes(
    unit: UnitId,
    phase_idx: usize,
    id_map: &UnitIdMap,
    commands: &mut Commands,
    log: &mut CombatLog,
    q: &mut Query<(
        &mut EnemyPhases,
        &mut Vital,
        &mut CombatStats,
        &mut Abilities,
        Option<&mut AxisProfile>,
        &mut Name,
        &mut Speed,
        Has<Dead>,
    )>,
    content: &ActiveContent,
    tag_cache: &AbilityTagCache,
    overrides: &mut Vec<PhaseOverrideIntent>,
    engine_state: &combat_engine::state::CombatState,
) {
    let Some(ent) = id_map.get_entity(unit) else {
        return;
    };
    let Ok((
        mut phases,
        mut vital,
        mut stats,
        mut abilities,
        role_opt,
        mut name,
        mut speed,
        is_dead,
    )) = q.get_mut(ent)
    else {
        return;
    };

    let Some(phase) = phases.pending.get(phase_idx).cloned() else {
        return;
    };

    // Capture name before mutation so the log shows the actual "was → now".
    let prev_name = name.as_str().to_string();

    if let Some(new_stats) = &phase.stats {
        *stats = new_stats.clone();
        vital.max_hp = new_stats.max_hp;
        // Clamp current HP to new max; heal_to_full overrides below.
        // project_state_to_ecs writes vital.hp from engine state (which already
        // committed the phase transition), but does NOT write vital.max_hp.
        vital.hp = vital.hp.min(vital.max_hp);
    }
    if phase.heal_to_full {
        vital.hp = vital.max_hp;
    }
    if is_dead && vital.hp > 0 {
        commands.entity(ent).remove::<Dead>();
    }
    if let Some(ref new_ability_ids) = phase.ability_ids {
        abilities.0 = new_ability_ids.clone();
    }

    // Mirror engine Unit.runtime → ECS so armor/magic_resist/Speed are up to date
    // for UI, legality checks (BevyActions), and AxisProfile inference below.
    // EnterPhase already ran in the engine before this system; reading runtime
    // here is the single source of truth (no second derivation).
    if let Some(engine_unit) = engine_state.unit(unit) {
        vital.armor = engine_unit.runtime.armor;
        vital.magic_resist = engine_unit.runtime.magic_resist;
        speed.0 = engine_unit.runtime.base_speed;
    }

    // Re-infer AxisProfile AFTER armor is updated so the profile reflects the
    // new defensive posture (armor was stale before this point).
    if let Some(mut role) = role_opt {
        if phase.stats.is_some() || phase.ability_ids.is_some() || phase.equipment.is_some() {
            *role = infer_profile(&abilities.0, vital.max_hp, vital.armor, content, tag_cache);
        }
    }

    let next_name = phase.name.clone().unwrap_or_else(|| prev_name.clone());
    if phase.name.is_some() {
        *name = Name::new(next_name.clone());
    }

    log.push(CombatEvent::PhaseEntered {
        actor: ent,
        prev_name,
        next_name: next_name.clone(),
        flavor: phase.flavor.clone(),
    });

    // Queue victory-override/deadline intent if the phase carries either field.
    if phase.victory_override.is_some() || phase.turn_limit.is_some() {
        overrides.push(PhaseOverrideIntent {
            entity: ent,
            victory_override: phase.victory_override.clone(),
            turn_limit: phase.turn_limit,
        });
    }

    // Insert AI behavior override component if the phase specifies one.
    if let Some(kind) = phase.ai_behavior {
        commands.entity(ent).insert(AiBehaviorOverride { kind });
    }

    // Mirror tag replacement into the ECS Tags component so Bevy-side legality
    // (BevyActions / ValidationTargetQ) doesn't read stale tags after the phase.
    // The engine already replaced Unit.tags in the EnterPhase arm (Slice C1);
    // this keeps the ECS copy in sync. None = keep existing Tags component unchanged.
    if let Some(ref new_tags) = phase.tags {
        commands.entity(ent).insert(Tags(new_tags.clone()));
    }

    // Pop exactly once per event (spec §8).
    phases.pending.remove(phase_idx);
}

/// Applies victory-override / deadline intents queued by phase transitions.
/// Runs in Execute right after `apply_bridge_queues_post_projection`.
pub fn apply_phase_overrides_system(
    mut queues: ResMut<BridgeQueues>,
    mut objective: ResMut<CombatObjective>,
    mut deadline: ResMut<PhaseDeadline>,
    ctx: Res<CombatContext>,
    mut ui_dirty: ResMut<UiDirty>,
    mut commands: Commands,
) {
    for intent in std::mem::take(&mut queues.phase_overrides) {
        if let Some(ov) = intent.victory_override {
            if let crate::content::encounters::VictoryCondition::KillTarget {
                marker_color, ..
            } = &ov
            {
                // The override always targets the phasing unit itself; load-time
                // validation (`validate_scenario`) guarantees the KillTarget enemy_name
                // equals the phasing enemy's config name. KillTarget victory is
                // marker-based (see `check_combat_end`), so attach the VictoryTarget
                // marker to the phasing entity unconditionally — its `target_alive` bool
                // and the UI ring then track the new objective. (Matching by display
                // `Name` would be wrong: combat names carry a race prefix, e.g.
                // "Зверокров Страж" vs the bare config name "Страж".)
                commands.entity(intent.entity).insert(VictoryTarget {
                    marker_color: *marker_color,
                });
            }
            objective.0 = ov;
            ui_dirty.0 |= UiDirtyFlags::PHASE_HINT;
        }
        if let Some(limit) = intent.turn_limit {
            deadline.0 = Some(PhaseDeadlineState {
                phase_started_round: ctx.round,
                limit,
            });
        }
    }
}
