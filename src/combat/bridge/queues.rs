//! Bridge side-effect queues (`BridgeQueues`) + their drain systems + engine-mirror resets.

use bevy::prelude::*;

use crate::app_state::CombatPhase;
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::content::content_view::ActiveContent;
use crate::game::combat_log::CombatLog;
use crate::game::components::{
    Abilities, ActiveCombatant, CombatStats, Dead, EnemyPhases, RuntimeStatsMirror, Vital,
};
use crate::game::messages::RestartCombat;
use crate::game::resources::{UiDirty, UiDirtyFlags};
use crate::ui::animation::{AnimationQueue, PendingAnim};

use super::*;
use combat_engine::state::UnitId;

/// Consolidated bridge-side-effect queues.
///
/// Groups all four formerly-separate `Pending*` Resources that had identical
/// shape (deferred vecs drained by apply systems in the `Execute` step).
/// Producers write into the relevant sub-field; the two apply systems drain
/// their respective halves before/after `project_state_to_ecs`.
///
/// Sub-fields:
/// * `deaths`          — units to mark `Dead` (pre-projection)
/// * `turn_lifecycle`  — `ActiveCombatant` inserts/removes + round-start flag (pre-projection)
/// * `animations`      — movement animations to push into `AnimationQueue` (post-projection)
/// * `phases`          — `(UnitId, phase_idx)` phase-transition pairs (post-projection)
/// * `phase_overrides` — victory-override/deadline intents queued by phase transitions (post-projection)
/// * `env_revealed`    — true when at least one `EnvRevealed` event fired this frame (post-projection)
#[derive(Resource, Default)]
pub struct BridgeQueues {
    pub deaths: Vec<UnitId>,
    pub turn_lifecycle: BridgeTurnLifecycle,
    pub animations: Vec<PendingAnim>,
    pub phases: Vec<(UnitId, usize)>,
    pub phase_overrides: Vec<PhaseOverrideIntent>,
    /// Set to `true` when an `EnvRevealed` engine event fires this step.
    /// Consumed in `apply_bridge_queues_post_projection` to trigger `HEX_FILL`
    /// so the trap tile appears immediately after reveal.
    pub env_revealed: bool,
}

/// Turn-lifecycle sub-queue inside [`BridgeQueues`].
///
/// Previously `PendingTurnLifecycle`.  Extracted as a named sub-struct so the
/// field types remain self-documenting without a top-level Resource.
#[derive(Default)]
pub struct BridgeTurnLifecycle {
    pub remove_active: Vec<UnitId>,
    pub insert_active: Vec<UnitId>,
    /// When true, a `RoundStarted` was seen this frame; a `StartRound` transition
    /// is scheduled by `apply_bridge_queues_pre_projection`.  `insert_active` is
    /// always drained in the same call — the `BumpRound`-settled actor is inserted
    /// via `insert_active` before `build_turn_order` runs in the next StartRound frame.
    pub round_started: bool,
}

/// Deferred victory-override / deadline intent emitted when a boss phase fires.
/// Consumed by `apply_phase_overrides_system` so `apply_phase_ecs_writes` (which
/// already has a 7-tuple query) need not also take the objective/deadline resources.
pub struct PhaseOverrideIntent {
    pub entity: Entity,
    pub victory_override: Option<crate::content::encounters::VictoryCondition>,
    pub turn_limit: Option<u32>,
}

// ── Queue Resources for deferred ECS side-effects ────────────────────────────

// ── apply-systems for the new queue Resources ─────────────────────────────────

/// Drains the pre-projection half of [`BridgeQueues`]: deaths and turn-lifecycle.
///
/// Runs after `process_action_system`, before `project_state_to_ecs`.
///
/// Turn-lifecycle drain order: `remove_active` first (evict old/skipped holder),
/// then `insert_active` (set new holder) → exactly one `ActiveCombatant` at all
/// times, no empty frame between remove and insert.
///
/// `round_started`: schedules the `StartRound` phase transition and resets the
/// flag.  `insert_active` is **always** drained — `BumpRound`'s `TurnStarted`
/// pushes the engine-settled actor into `insert_active`, and `build_turn_order`
/// no longer does a blanket `remove::<ActiveCombatant>`, so draining here is
/// safe and correct for both round-boundary and mid-round handoffs.
pub fn apply_bridge_queues_pre_projection(
    mut queues: ResMut<BridgeQueues>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut next_phase: Option<ResMut<NextState<CombatPhase>>>,
) {
    // Deaths
    for uid in std::mem::take(&mut queues.deaths) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).insert(Dead);
        }
    }

    // Turn lifecycle — remove before insert to maintain exactly-one invariant.
    for uid in std::mem::take(&mut queues.turn_lifecycle.remove_active) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).remove::<ActiveCombatant>();
        }
    }

    if queues.turn_lifecycle.round_started {
        // Schedule the StartRound phase transition; reset the flag.
        // insert_active is drained below (same path as mid-round) so the
        // BumpRound-settled actor gets ActiveCombatant before StartRound runs.
        if let Some(ref mut np) = next_phase {
            np.set(CombatPhase::StartRound);
        }
        queues.turn_lifecycle.round_started = false;
    }

    // Always drain insert_active (covers both mid-round handoff and round-boundary).
    for uid in std::mem::take(&mut queues.turn_lifecycle.insert_active) {
        if let Some(ent) = id_map.get_entity(uid) {
            commands.entity(ent).insert(ActiveCombatant);
        }
    }
}

/// Drains the post-projection half of [`BridgeQueues`]: animations and phase transitions.
///
/// Runs after `project_state_to_ecs`, before `flush_pending_ai_log_system`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_bridge_queues_post_projection(
    mut queues: ResMut<BridgeQueues>,
    id_map: Res<UnitIdMap>,
    mut commands: Commands,
    mut log: ResMut<CombatLog>,
    active_content: Res<ActiveContent>,
    tag_cache: Res<AbilityTagCache>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut dirty: ResMut<UiDirty>,
    combat_state: Res<CombatStateRes>,
    mut q: Query<(
        &mut EnemyPhases,
        &mut Vital,
        &mut CombatStats,
        &mut Abilities,
        Option<&mut AxisProfile>,
        &mut Name,
        &mut RuntimeStatsMirror,
        Has<Dead>,
    )>,
) {
    // Animations
    for anim in std::mem::take(&mut queues.animations) {
        anim_queue.0.push_back(anim);
    }

    // EnvRevealed: trigger HEX_FILL so a newly-revealed (still-armed) trap tile
    // renders. Reserved for the reveal mechanic (e.g. a scout spotting traps);
    // firing a trap removes it, so triggering does not emit EnvRevealed.
    if std::mem::take(&mut queues.env_revealed) {
        dirty.0 |= UiDirtyFlags::HEX_FILL;
    }

    // Phase transitions — move phases out first so we can borrow phase_overrides independently.
    let transitions = std::mem::take(&mut queues.phases);
    for (unit, phase_idx) in transitions {
        apply_phase_ecs_writes(
            unit,
            phase_idx,
            &id_map,
            &mut commands,
            &mut log,
            &mut q,
            &active_content,
            &tag_cache,
            &mut queues.phase_overrides,
            &combat_state.0,
        );
    }
}

// ── reset_engine_mirrors ──────────────────────────────────────────────────────

/// Clears the engine-side mirrors (`CombatStateRes`, `UnitIdMap`,
/// `BridgeQueues`) so a fresh combat starts from a clean slate.
///
/// Without this reset, the next combat's `StartRound` system
/// `project_state_to_ecs` would iterate stale unit data from the previous
/// combat and try to write its positions into the freshly-cleared
/// `HexPositions` resource, colliding with the newly-spawned combatants.
///
/// Plain helper — both reset systems below delegate here so the "what counts
/// as an engine mirror" knowledge lives in one place. Add a new mirror? Update
/// this function only.
fn reset_engine_mirrors(
    combat_state: &mut CombatStateRes,
    id_map: &mut UnitIdMap,
    queues: &mut BridgeQueues,
) {
    *combat_state = CombatStateRes::default();
    id_map.clear();
    *queues = BridgeQueues::default();
}

/// `OnExit(AppState::Combat)` system — natural combat-end teardown.
pub fn reset_engine_mirrors_on_exit_combat(
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut queues: ResMut<BridgeQueues>,
) {
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut queues);
}

/// `Update` system listening to `RestartCombat` messages. The restart flow
/// keeps `AppState::Combat`, so `OnExit` doesn't fire — we need an explicit
/// reader. Bevy permits multiple independent readers of the same message
/// stream, so this coexists with `restart_combat_system` (each has its own
/// cursor).
pub fn reset_engine_mirrors_on_restart(
    mut reader: MessageReader<RestartCombat>,
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut queues: ResMut<BridgeQueues>,
) {
    if reader.read().next().is_none() {
        return;
    }
    reset_engine_mirrors(&mut combat_state, &mut id_map, &mut queues);
}
