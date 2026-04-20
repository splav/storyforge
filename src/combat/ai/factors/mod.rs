//! Per-step 9-factor computation.
//!
//! Produces `[damage, kill, cc, heal, position, risk, focus, intent, scarcity]`
//! for a single `ScoredStep`. Normalisation, role-axis weighting and the
//! plan-level aggregation (discounted sums, max-across-steps) live in
//! `combat::ai::planning::scorer`.
//!
//! Module layout:
//! - `offensive` — damage / heal / kill / cc (single-target and AoE), `aoe_area`.
//! - `scarcity`  — resource-vs-swing scoring for Cast candidates.
//! - `adjustments` — reservation nerfs + crit-fail expected-value adjustment.

#![allow(clippy::too_many_arguments)]

mod adjustments;
mod aoe_hits;
mod offensive;
mod scarcity;

pub use adjustments::crit_fail_adjusted;
pub use aoe_hits::{aoe_hits, AoeHits};
pub use offensive::aoe_area;

use crate::combat::ai::planning::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::ScoringCtx;
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

// ── Scored step ─────────────────────────────────────────────────────────────

/// A single plan step as seen by the scoring layer — a lightweight ref-based
/// view over `PlanStep` plus the caster position that step happens *at*.
///
/// Replaces the owned `ActionCandidate` that used to pivot between planning
/// and scoring. Scoring now pays zero allocations per step; debug walks
/// `TurnPlan` directly.
///
/// For `Cast`: `caster_tile` is the actor's tile when the spell fires (the
/// actor doesn't move during a pure cast). For `Move`: `caster_tile` is the
/// *destination* — position/risk factors are keyed off the tile the actor
/// ends up on, not the one it's leaving.
pub enum ScoredStep<'a> {
    Cast {
        ability: &'a AbilityId,
        target: Entity,
        target_pos: Hex,
        caster_tile: Hex,
    },
    Move {
        caster_tile: Hex,
    },
}

impl<'a> ScoredStep<'a> {
    pub fn caster_tile(&self) -> Hex {
        match self {
            Self::Cast { caster_tile, .. } | Self::Move { caster_tile } => *caster_tile,
        }
    }

    pub fn target(&self) -> Option<Entity> {
        match self {
            Self::Cast { target, .. } => Some(*target),
            Self::Move { .. } => None,
        }
    }

    pub fn ability(&self) -> Option<&AbilityId> {
        match self {
            Self::Cast { ability, .. } => Some(*ability),
            Self::Move { .. } => None,
        }
    }

    pub fn is_move_only(&self) -> bool {
        matches!(self, Self::Move { .. })
    }

    /// Build from a `PlanStep`. `pre_step_pos` is where the actor stood right
    /// before this step; for `Move`, the tile auto-advances to the path's
    /// destination so position factors see the endpoint.
    pub fn from_plan_step(step: &'a PlanStep, pre_step_pos: Hex) -> Self {
        match step {
            PlanStep::Cast { ability, target, target_pos } => Self::Cast {
                ability,
                target: *target,
                target_pos: *target_pos,
                caster_tile: pre_step_pos,
            },
            PlanStep::Move { path } => Self::Move {
                caster_tile: *path.last().unwrap_or(&pre_step_pos),
            },
        }
    }

    /// Build the view of what `commit_plan` would actually execute this tick
    /// — first step for solo or leading move, bundled Cast when preceded by
    /// a Move. Used by the debug formatter.
    pub fn from_plan_committed(plan: &'a TurnPlan, actor_pos: Hex) -> Self {
        // Bundling rule comes from `TurnPlan::committed_prefix` — one source
        // of truth shared with `commit_plan` and `committed_step_count`.
        match plan.committed_prefix() {
            CommittedPrefix::EndTurn => Self::Move { caster_tile: actor_pos },
            CommittedPrefix::Cast { ability, target, target_pos } => Self::Cast {
                ability,
                target,
                target_pos,
                caster_tile: actor_pos,
            },
            CommittedPrefix::MoveThenCast { path, ability, target, target_pos } => {
                let dest = path.last().copied().unwrap_or(actor_pos);
                Self::Cast {
                    ability,
                    target,
                    target_pos,
                    caster_tile: dest,
                }
            }
            CommittedPrefix::MoveOnly { path } => {
                let dest = path.last().copied().unwrap_or(actor_pos);
                Self::Move { caster_tile: dest }
            }
        }
    }
}

// ── Factor layout ───────────────────────────────────────────────────────────

/// 9 utility factors: damage, kill, cc, heal, position, risk, focus, intent, scarcity.
pub const NUM_FACTORS: usize = 9;

// Named indices into the factor array. Use these anywhere a factor is read
// by number — `raw[DAMAGE_IDX]` makes intent obvious and makes a future
// reorder impossible to miss. The definitional array in
// `scorer::compute_plan_factors_sans_intent` stays positional on purpose
// (it's the one place *declaring* the layout).
pub const DAMAGE_IDX: usize = 0;
pub const KILL_IDX: usize = 1;
pub const CC_IDX: usize = 2;
pub const HEAL_IDX: usize = 3;
pub const POSITION_IDX: usize = 4;
pub const RISK_IDX: usize = 5;
pub const FOCUS_IDX: usize = 6;
pub const INTENT_IDX: usize = 7;
pub const SCARCITY_IDX: usize = 8;

/// Factors that can be negative (position, intent, scarcity).
/// These use symmetric normalization in `planning::scorer`: divide by
/// `max(|min|, |max|)` → [-1, 1]. Non-negative factors use max normalization
/// → [0, 1].
pub const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, true, false, false, true, true,
];

/// Per-plan utility factors as a named struct. Replaces ad-hoc
/// `[f32; NUM_FACTORS]` indexing throughout the scoring pipeline so callers
/// read `f.intent` instead of `f[INTENT_IDX]`. Layout matches the
/// `as_array()` order — the one place that **declares** the layout.
///
/// Numeric work (batch normalization in `finalize_scores`) still goes
/// through `[f32; NUM_FACTORS]` views via `as_array()` / `from_array()`, so
/// SIMD/loop-based math stays cheap. Log + debug writers convert to the
/// stable `[f32; 9]` wire format at the boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanFactors {
    pub damage: f32,
    pub kill: f32,
    pub cc: f32,
    pub heal: f32,
    pub position: f32,
    pub risk: f32,
    pub focus: f32,
    pub intent: f32,
    pub scarcity: f32,
}

impl PlanFactors {
    pub fn as_array(&self) -> [f32; NUM_FACTORS] {
        [
            self.damage, self.kill, self.cc, self.heal, self.position,
            self.risk, self.focus, self.intent, self.scarcity,
        ]
    }

    pub fn from_array(a: [f32; NUM_FACTORS]) -> Self {
        Self {
            damage: a[DAMAGE_IDX],
            kill: a[KILL_IDX],
            cc: a[CC_IDX],
            heal: a[HEAL_IDX],
            position: a[POSITION_IDX],
            risk: a[RISK_IDX],
            focus: a[FOCUS_IDX],
            intent: a[INTENT_IDX],
            scarcity: a[SCARCITY_IDX],
        }
    }
}

/// Per-step offensive factors (populated only for Cast).
#[derive(Default)]
pub(super) struct OffensiveFactors {
    pub(super) damage: f32,
    pub(super) heal: f32,
    pub(super) kill: f32,
    pub(super) cc: f32,
}

/// Compute the 9 raw utility factors for a single scored step — **excluding**
/// the intent factor. `factor[7]` (intent) is returned as `0.0` and aggregated
/// separately at the plan level by `scorer::compute_plan_intent_sum`. This
/// split lets the utility pipeline cache the intent-independent factors once
/// per plan and re-apply a new intent (viability fallback, LastStand rescore)
/// without redoing damage/heal/kill/cc/position/risk/focus/scarcity math.
///
/// Axes: [damage, kill, cc, heal, position, risk, focus, 0.0, scarcity].
pub fn compute_factors(ctx: &ScoringCtx, step: &ScoredStep) -> PlanFactors {
    let tile = step.caster_tile();
    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;

    let mut off = match step {
        ScoredStep::Cast { ability, target_pos, target, caster_tile } => {
            offensive::compute_offensive(
                ability, *target_pos, *target, *caster_tile, active, ctx.utility, snap,
            )
        }
        ScoredStep::Move { .. } => OffensiveFactors::default(),
    };

    let mut position = evaluate_position(tile, &active.role, maps);
    let risk = 1.0 - maps.danger.get(tile);
    let mut focus = step
        .target()
        .and_then(|t| snap.unit(t))
        .map(|t| target_priority(active, t, snap))
        .unwrap_or(0.0);

    adjustments::apply_reservation_adjustments(
        step, &mut off, &mut focus, &mut position, snap, ctx.utility, ctx.reservations,
    );

    let scarcity = match step {
        ScoredStep::Cast { .. } => {
            scarcity::compute_scarcity(step, active, off.kill, ctx.utility, snap)
        }
        ScoredStep::Move { .. } => 0.0,
    };

    PlanFactors {
        damage: off.damage,
        kill: off.kill,
        cc: off.cc,
        heal: off.heal,
        position,
        risk,
        focus,
        intent: 0.0, // intent — filled in by `compute_plan_intent_sum` when needed
        scarcity,
    }
}

#[cfg(test)]
mod tests {
    // ── Normalization tests ───────────────────────────────────────────

    #[test]
    fn signed_normalization_preserves_negative_order() {
        let values = [-3.0f32, -1.0, -0.5];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let normalized: Vec<f32> = values.iter().map(|v| v / max_abs).collect();
        assert_eq!(normalized, vec![-1.0, -1.0 / 3.0, -0.5 / 3.0]);
        assert!(normalized[0] < normalized[1]);
        assert!(normalized[1] < normalized[2]);
    }

    #[test]
    fn signed_normalization_flat_batch_gives_zero() {
        let values = [0.0f32; 3];
        let max_abs = values.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        for &v in &values {
            let norm = if max_abs > f32::EPSILON { v / max_abs } else { 0.0 };
            assert_eq!(norm, 0.0);
            assert!(!norm.is_nan());
        }
    }
}
