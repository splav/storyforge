//! Per-step 10-factor computation.
//!
//! Produces `[damage, kill_now, kill_promised, cc, heal, intent, scarcity, tempo_gain, saturation, self_survival]`
//! for a single `ScoredStep`. Normalisation, role-axis weighting and the
//! plan-level aggregation (discounted sums, max-across-steps) live in
//! `combat::ai::planning::scorer`.
//!
//! Module layout:
//! - `offensive` — damage / heal / kill_now / kill_promised / cc (single-target and AoE), `aoe_area`.
//! - `scarcity`  — resource-vs-swing scoring for Cast candidates.
//! - `adjustments` — reservation nerfs + crit-fail expected-value adjustment.
//! - `tempo`     — plan-terminal `tempo_gain`.
//! - `saturation` — per-plan buff-redundancy penalty (same class, same target).
//! - `survival`  — plan-level `self_survival` (heal + armor-buff + exit-danger).
//!
//! Phase 6 removed the legacy `position`, `risk`, and `focus` axes. Their
//! signals are now covered by `tempo_gain` (approach + exit-danger) and
//! `self_survival` (per-path bleed via AoO exposure). `evaluate_position`
//! in `position_eval.rs` is kept as a helper for `Reposition` intent scoring
//! and influence-map debugging, but is no longer a scored factor.

mod adjustments;
mod aoe_hits;
mod offensive;
mod saturation;
mod scarcity;
mod survival;
mod tempo;

pub use adjustments::crit_fail_adjusted;
pub use aoe_hits::{aoe_hits, AoeHits};
pub use offensive::aoe_area;
pub use saturation::buff_saturation_penalty;
pub use survival::compute_plan_self_survival;
pub use tempo::compute_plan_tempo_gain;

use crate::combat::ai::planning::types::{CommittedPrefix, PlanStep, TurnPlan};
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

/// 10 utility factors: damage, kill_now, kill_promised, cc, heal, intent, scarcity, tempo_gain, saturation, self_survival.
pub const NUM_FACTORS: usize = 10;

// Named indices into the factor array. Use these anywhere a factor is read
// by number — `raw[DAMAGE_IDX]` makes intent obvious and makes a future
// reorder impossible to miss. The definitional array in
// `scorer::compute_plan_factors_sans_intent` stays positional on purpose
// (it's the one place *declaring* the layout).
pub const DAMAGE_IDX: usize = 0;
pub const KILL_NOW_IDX: usize = 1;
pub const KILL_PROMISED_IDX: usize = 2;
pub const CC_IDX: usize = 3;
pub const HEAL_IDX: usize = 4;
pub const INTENT_IDX: usize = 5;
pub const SCARCITY_IDX: usize = 6;
pub const TEMPO_IDX: usize = 7;
pub const SATURATION_IDX: usize = 8;
pub const SELF_SURVIVAL_IDX: usize = 9;

/// Factors that can be negative (intent, scarcity, tempo_gain, saturation, self_survival).
/// These use symmetric normalization in `planning::scorer`: divide by
/// `max(|min|, |max|)` → [-1, 1]. Non-negative factors use max normalization
/// → [0, 1].
pub const SIGNED_FACTOR: [bool; NUM_FACTORS] = [
    false, false, false, false, false, true, true, true, true, true,
];

/// Per-plan utility factors as a named struct. Replaces ad-hoc
/// `[f32; NUM_FACTORS]` indexing throughout the scoring pipeline so callers
/// read `f.intent` instead of `f[INTENT_IDX]`. Layout matches the
/// `as_array()` order — the one place that **declares** the layout.
///
/// Numeric work (batch normalization in `finalize_scores`) still goes
/// through `[f32; NUM_FACTORS]` views via `as_array()` / `from_array()`, so
/// SIMD/loop-based math stays cheap. Log + debug writers convert to the
/// stable `[f32; 10]` wire format at the boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanFactors {
    pub damage: f32,
    pub kill_now: f32,
    pub kill_promised: f32,
    pub cc: f32,
    pub heal: f32,
    pub intent: f32,
    pub scarcity: f32,
    pub tempo_gain: f32,
    pub saturation: f32,
    pub self_survival: f32,
}

impl PlanFactors {
    pub fn as_array(&self) -> [f32; NUM_FACTORS] {
        [
            self.damage, self.kill_now, self.kill_promised, self.cc, self.heal,
            self.intent, self.scarcity, self.tempo_gain, self.saturation, self.self_survival,
        ]
    }

    pub fn from_array(a: [f32; NUM_FACTORS]) -> Self {
        Self {
            damage: a[DAMAGE_IDX],
            kill_now: a[KILL_NOW_IDX],
            kill_promised: a[KILL_PROMISED_IDX],
            cc: a[CC_IDX],
            heal: a[HEAL_IDX],
            intent: a[INTENT_IDX],
            scarcity: a[SCARCITY_IDX],
            tempo_gain: a[TEMPO_IDX],
            saturation: a[SATURATION_IDX],
            self_survival: a[SELF_SURVIVAL_IDX],
        }
    }
}

/// Per-step offensive factors (populated only for Cast).
#[derive(Default)]
pub(super) struct OffensiveFactors {
    pub(super) damage: f32,
    pub(super) heal: f32,
    pub(super) kill_now: f32,
    pub(super) kill_promised: f32,
    pub(super) cc: f32,
}

/// Compute the per-step raw utility factors — **excluding** intent, tempo_gain,
/// and self_survival (all three are filled plan-level by their respective
/// compute_plan_* functions). `factor[INTENT_IDX]` is returned as `0.0` and
/// aggregated separately at the plan level by `scorer::compute_plan_intent_sum`.
/// This split lets the utility pipeline cache the intent-independent factors
/// once per plan and re-apply a new intent (viability fallback, LastStand
/// rescore) without redoing damage/heal/kill/cc/scarcity.
///
/// Axes: [damage, kill_now, kill_promised, cc, heal, 0.0, scarcity, 0.0, 0.0, 0.0].
pub fn compute_factors(ctx: &ScoringCtx, step: &ScoredStep) -> PlanFactors {
    let mut off = match step {
        ScoredStep::Cast { ability, target_pos, target, caster_tile } => {
            offensive::compute_offensive(ability, *target_pos, *target, *caster_tile, ctx)
        }
        ScoredStep::Move { .. } => OffensiveFactors::default(),
    };

    adjustments::apply_reservation_adjustments(step, &mut off, ctx);

    let scarcity = match step {
        ScoredStep::Cast { .. } => scarcity::compute_scarcity(step, off.kill_now, ctx),
        ScoredStep::Move { .. } => 0.0,
    };

    PlanFactors {
        damage: off.damage,
        kill_now: off.kill_now,
        kill_promised: off.kill_promised,
        cc: off.cc,
        heal: off.heal,
        intent: 0.0,        // filled in by `compute_plan_intent_sum` when needed
        scarcity,
        tempo_gain: 0.0,    // filled in by `compute_plan_tempo_gain` when needed
        saturation: 0.0,    // filled in by scorer's per-plan saturation loop
        self_survival: 0.0, // filled in by `compute_plan_self_survival` when needed
    }
}

// Normalization tests used to live here but only exercised inlined copies
// of the formula, not production code. The real batch-normalisation contract
// is pinned by `planning::scorer::tests::sum_factors_scale_by_step_weight`
// and `rescore_matches_full_score_under_same_intent`, which drive
// `finalize_scores` end-to-end.
