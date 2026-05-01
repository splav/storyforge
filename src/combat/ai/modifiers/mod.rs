//! Post-normalisation plan modifiers — step 8.B.
//!
//! Each `PlanModifier` contributes a signed addendum to `ann.score` after
//! the full factor/terminal scoring pass. Three built-in modifiers are
//! registered in `PLAN_MODIFIERS` (apply order is fixed):
//!
//! 1. `summon_bonus` — scarce-resource bonus for Summon plans.
//! 2. `trade_bonus`  — economic gain/loss relative to actor value.
//! 3. `repair_bonus` — goal-affinity amplifier when a stored goal is present.
//!
//! ## Pipeline integration
//!
//! `PlanModifiersStage` applies these modifiers in the `run_pool_pipeline`
//! between `RepairAffinityStage` (which populates `ann.repair_affinity`) and
//! `PickBestStage` (which selects the winner). Modifiers run after all rescoring
//! stages (`ViabilityStage`, `ModeSelectionStage`, `FinalizeStage`) so they see the final
//! `finalize_scores` output. Results are recorded in `PlanAnnotation.modifiers`
//! for observability — one `ModifierContribution` entry per modifier.

pub mod repair_bonus;
pub mod summon_bonus;
pub mod trade_bonus;

use crate::combat::ai::pipeline::StageCtx;
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::repair::RepairWeights;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A post-normalisation additive modifier applied to every plan's score.
///
/// Implementations must be `Sync` so they can live in a `static` slice.
/// All three built-in implementations are zero-state unit structs —
/// `Sync` is satisfied automatically.
pub trait PlanModifier: Sync {
    /// Stable identifier for logging and debug overlays.
    fn name(&self) -> &'static str;

    /// Compute the signed additive contribution for one plan.
    ///
    /// Returns `0.0` when the modifier does not apply (e.g. no Summon steps,
    /// no stored goal). Positive values increase `ann.score`; negative values
    /// decrease it.
    fn modify(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ModifierCtx<'_, '_, '_>,
    ) -> f32;
}

// ── Context ──────────────────────────────────────────────────────────────────

/// Read-only context passed to every `PlanModifier::modify` call.
///
/// Lifetime parameters:
/// - `'w` — world/map borrows inside `StageCtx::scoring` (`AiWorld`, `InfluenceMaps`).
/// - `'s` — outer `pick_action` stack-frame borrow (`ScoringCtx`, intent, rng).
/// - `'a` — the borrow of `StageCtx` itself (shorter than both `'w` and `'s`).
pub struct ModifierCtx<'w, 's, 'a> {
    pub stage: &'a StageCtx<'w, 's>,
    /// Pre-computed per-template summon DPR cache, built once per pool in
    /// `PlanModifiersStage`. Empty when no plan summons.
    pub summon_dpr: &'a HashMap<String, f32>,
    /// `unit_value(active, world.content)` — computed once per pool.
    pub actor_value: f32,
    /// Role-mixed repair weights for the active actor. Computed once per pool
    /// via `active.role.repair_weights(world.tuning)`.
    pub repair_weights: RepairWeights,
}

// ── Contribution record ───────────────────────────────────────────────────────

/// Per-modifier additive contribution stored in `PlanAnnotation.modifiers`.
///
/// Populated by `PlanModifiersStage` for each plan. `name` matches the
/// `PlanModifier::name()` return value; `contribution` is the signed addendum
/// applied to `ann.score`. Entries appear in `PLAN_MODIFIERS` order.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ModifierContribution {
    pub name: String,
    pub contribution: f32,
}

// ── Static registry ──────────────────────────────────────────────────────────

/// Ordered slice of all active plan modifiers.
///
/// Order is fixed: `[summon_bonus, trade_bonus, repair_bonus]`.
/// `PlanModifiersStage` applies them left-to-right; the same order
/// appears in `PlanAnnotation.modifiers` entries.
pub static PLAN_MODIFIERS: &[&dyn PlanModifier] = &[
    &summon_bonus::MODIFIER,
    &trade_bonus::MODIFIER,
    &repair_bonus::MODIFIER,
];
