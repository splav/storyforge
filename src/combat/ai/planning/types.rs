//! Plan data model: steps, multi-step turns, and the cumulative outcome of
//! applying a plan to a simulated battle state.

use crate::combat::ai::snapshot::BattleSnapshot;
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

/// One atomic action inside a turn plan.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum PlanStep {
    /// Walk along `path`. `path` excludes the starting tile and includes the
    /// destination — matches the convention used by `ReachableMap::path_to`
    /// and `MoveUnit { path }`. `path.len()` therefore equals MP cost.
    Move {
        #[serde(with = "crate::combat::ai::serde_helpers::hex_vec")]
        path: Vec<Hex>,
    },
    /// Cast `ability` at `target` (living entity) on `target_pos` (cell the
    /// primary effect is centred on; for AoE this is the blast origin).
    Cast {
        ability: AbilityId,
        #[serde(with = "crate::combat::ai::serde_helpers::entity")]
        target: Entity,
        #[serde(with = "crate::combat::ai::serde_helpers::hex")]
        target_pos: Hex,
    },
}

/// A candidate plan for a whole turn (1..=max_depth steps). Scored as a unit;
/// only the first step is committed per tick, and next tick either validates &
/// continues or replans.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TurnPlan {
    pub steps: Vec<PlanStep>,
    /// Actor's position after all steps.
    #[serde(with = "crate::combat::ai::serde_helpers::hex")]
    pub final_pos: Hex,
    /// AP remaining after all steps.
    pub residual_ap: i32,
    /// MP remaining after all steps.
    pub residual_mp: i32,
    /// Per-step outcomes aggregated during generation; consumed by Phase 3
    /// scoring to compute the final factors without re-running sim.
    pub outcomes: Vec<StepOutcome>,
    /// Cheap proxy score used for beam-search pruning only. The final score
    /// and factor decomposition are produced in Phase 3.
    pub partial_score: f32,
    /// Sim snapshot cached after each applied step. `sim_snapshots[k]` is the
    /// world state AFTER `steps[0..=k]` have been simulated. The "pre-step-k"
    /// snapshot a scorer needs is `sim_snapshots[k-1]` for k>0, or the
    /// original snapshot for k=0. Invariant: `sim_snapshots.len() == steps.len()`.
    ///
    /// Populated inside `generate_plans` (we already ran the sim there to
    /// produce `outcomes`; caching the resulting state costs one `.clone()`
    /// per step). Consumed by `compute_plan_factors` so it doesn't re-simulate
    /// the whole plan a second time. Runtime-only; excluded from the AI log
    /// because snapshots are derivable from `snapshot + steps`.
    #[serde(skip)]
    pub sim_snapshots: Vec<BattleSnapshot>,
}

/// Effects produced by a single simulated step. Used by scoring to accumulate
/// per-plan factors (damage/kill/heal/cc totals, worst-path danger, etc.).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct StepOutcome {
    /// Expected HP-equivalent damage dealt (post-armor / post-vulnerability).
    pub damage: f32,
    /// Expected HP-equivalent healing done.
    pub heal: f32,
    /// Targets whose HP dropped to 0 during this step (ordered by application).
    #[serde(with = "crate::combat::ai::serde_helpers::entity_vec")]
    pub killed: Vec<Entity>,
    /// Entities that received a turn-skipping status (stun, paralyse, sleep).
    #[serde(with = "crate::combat::ai::serde_helpers::entity_vec")]
    pub stunned: Vec<Entity>,
    /// Number of targets touched by the step (AoE or single). Zero for Move.
    pub hits: u32,
    /// True if the step was a Move.
    pub moved: bool,
}
