//! Factor registry and per-step/plan/terminal computation (post-8.A).
//!
//! ## Architecture
//!
//! Three enum-driven registries replace ad-hoc positional arrays:
//! - `StepFactor` (7 variants) — per-step axes: damage, kill_now, kill_promised, cc, heal, scarcity, saturation.
//! - `PlanFactor` (3 variants) — plan-level axes: intent, tempo_gain, self_survival.
//! - `TerminalFactor` (8 variants) — terminal-state axes: exposure_at_end, …, pressure_spacing_zone.
//!
//! `PlanFactorValues` is the typed `[f32; 10]` wrapper (step slots 0..7, plan slots 7..10)
//! with custom named-map serde (schema v29). `TerminalScore` lives in `factors::terminal`.
//!
//! ## Module layout
//! - `registry`   — `factor_kind!` macro, `BatchStats`, `NeedAxis`, `default_norm`.
//! - `step/`      — 7 per-step leaf modules + `StepFactor` enum (each leaf owns its implementation).
//! - `plan/`      — 3 plan-level leaf modules + `PlanFactor` enum (each leaf owns its implementation).
//! - `terminal/`  — 8 terminal leaf modules + `TerminalFactor` enum + `TerminalScore` wrapper.
//! - `offensive`  — shared `compute_offensive` helper (used by step leaves), `aoe_area`.
//! - `adjustments`— reservation nerfs + crit-fail expected-value adjustment.

mod adjustments;
pub(crate) mod aoe_hits;
pub(crate) mod offensive;

// ── Registry modules ──────────────────────────────────────────────────────────
pub mod aggregate;
pub mod plan;
pub mod registry;
pub mod step;
pub mod terminal;
pub mod terminal_state;

pub use adjustments::crit_fail_adjusted;
pub use aoe_hits::{aoe_hits, AoeHits};
pub use offensive::aoe_area;
pub use plan::self_survival::compute_plan_self_survival;
pub use plan::tempo_gain::compute_plan_tempo_gain;

// ── Aggregate re-exports ─────────────────────────────────────────────────────
pub use aggregate::{
    build_summon_dpr_cache, compute_plan_factors, compute_plan_factors_sans_intent,
    compute_plan_intent_sum, factor_contribution, finalize_scores, rescore_with_intent,
    rescore_with_per_plan_modes, score_plans_with_raw, worst_path_danger,
};
pub use terminal_state::terminal_state_score;

// ── Registry re-exports (commit 1) ───────────────────────────────────────────
pub use plan::PlanFactor;
pub use registry::{BatchStats, NeedAxis, default_norm};
pub use step::StepFactor;
pub use terminal::{TerminalFactor, TerminalScore as FactorTerminalScore};

use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::plan::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::orchestration::ScoringCtx;
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

/// Per-step offensive factors (populated only for Cast).
#[derive(Default)]
pub(crate) struct OffensiveFactors {
    pub(crate) damage: f32,
    pub(crate) heal: f32,
    pub(crate) kill_now: f32,
    pub(crate) kill_promised: f32,
    pub(crate) cc: f32,
}

/// Compute the offensive sub-factors for a single Cast step, including
/// reservation adjustments. Used by the per-`StepFactor` leaf modules in
/// `factors/step/` to avoid duplicating the offensive math.
///
/// Returns `OffensiveFactors::default()` for `Move` steps.
pub(crate) fn compute_offensive_for_step(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    outcome: &ActionOutcomeEstimate,
) -> OffensiveFactors {
    let mut off = match step {
        ScoredStep::Cast { ability, target_pos, target, caster_tile } => {
            offensive::compute_offensive(ability, *target_pos, *target, *caster_tile, ctx, outcome)
        }
        ScoredStep::Move { .. } => OffensiveFactors::default(),
    };
    adjustments::apply_reservation_adjustments(step, &mut off, ctx);
    off
}


// Normalization tests used to live here but only exercised inlined copies
// of the formula, not production code. The real batch-normalisation contract
// is pinned by `planning::scorer::tests::sum_factors_scale_by_step_weight`
// and `rescore_matches_full_score_under_same_intent`, which drive
// `finalize_scores` end-to-end.

// ── PlanFactorValues typed wrapper ───────────────────────────────────────────

/// Typed `[f32; 10]` wrapper for plan factor values.
///
/// Layout: `[damage(0), kill_now(1), kill_promised(2), cc(3), heal(4),
///           scarcity(5), saturation(6), intent(7), tempo_gain(8), self_survival(9)]`.
///
/// Slots 0..7 are `StepFactor` values (discounted per-step sums); slots 7..10
/// are `PlanFactor` values (plan-level aggregates).
///
/// `get`/`set` use `StepFactor as usize` directly; `get_plan`/`set_plan` add
/// `StepFactor::count()` as the offset.
///
/// Custom serde writes/reads a named map `{"damage": ..., ..., "self_survival":
/// ...}` via `StepFactor::from_name` + `PlanFactor::from_name`. Unknown keys →
/// error; missing keys → 0.0 (forward-compat).
///
/// # Column order (post-8.A)
/// `[damage, kill_now, kill_promised, cc, heal, scarcity, saturation, intent,
/// tempo_gain, self_survival]`.
/// Was `[damage, kill_now, kill_promised, cc, heal, intent, scarcity,
/// tempo_gain, saturation, self_survival]` in v28. The re-permutation
/// co-incides with TOML row update in commit 2.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PlanFactorValues([f32; 10]);

impl PlanFactorValues {
    /// Get a step-factor slot by enum discriminant.
    pub fn get(&self, f: StepFactor) -> f32 {
        self.0[f as usize]
    }

    /// Get a plan-factor slot by enum discriminant.
    pub fn get_plan(&self, f: PlanFactor) -> f32 {
        self.0[StepFactor::count() + f as usize]
    }

    /// Set a step-factor slot.
    pub fn set(&mut self, f: StepFactor, v: f32) {
        self.0[f as usize] = v;
    }

    /// Set a plan-factor slot.
    pub fn set_plan(&mut self, f: PlanFactor, v: f32) {
        self.0[StepFactor::count() + f as usize] = v;
    }

    /// Return the underlying `[f32; 10]` in column order.
    pub fn as_array(&self) -> [f32; 10] {
        self.0
    }
}

impl serde::Serialize for PlanFactorValues {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let total = StepFactor::count() + PlanFactor::count();
        let mut map = ser.serialize_map(Some(total))?;
        for f in StepFactor::iter() {
            map.serialize_entry(f.name(), &self.get(f))?;
        }
        for f in PlanFactor::iter() {
            map.serialize_entry(f.name(), &self.get_plan(f))?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for PlanFactorValues {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = PlanFactorValues;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a map of factor name to f32")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<PlanFactorValues, A::Error> {
                let mut out = PlanFactorValues::default();
                while let Some(key) = map.next_key::<String>()? {
                    let val: f32 = map.next_value()?;
                    if let Some(f) = StepFactor::from_name(&key) {
                        out.set(f, val);
                    } else if let Some(f) = PlanFactor::from_name(&key) {
                        out.set_plan(f, val);
                    } else {
                        return Err(serde::de::Error::custom(format!(
                            "unknown factor \"{key}\""
                        )));
                    }
                }
                Ok(out)
            }
        }
        de.deserialize_map(Visitor)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::factors::plan::PlanFactor;
    use crate::combat::ai::scoring::factors::step::StepFactor;
    use crate::combat::ai::scoring::factors::terminal::TerminalFactor;

    // ── factor_kind macro metadata ────────────────────────────────────────────

    #[test]
    fn factor_kind_macro_generates_correct_enum_metadata() {
        // StepFactor: 7 variants
        assert_eq!(StepFactor::count(), 7);
        let step_names: Vec<_> = StepFactor::iter().map(|f| f.name()).collect();
        assert_eq!(
            step_names,
            ["damage", "kill_now", "kill_promised", "cc", "heal", "scarcity", "saturation"]
        );
        // signed flags
        assert!(!StepFactor::Damage.signed());
        assert!(!StepFactor::KillNow.signed());
        assert!(!StepFactor::KillPromised.signed());
        assert!(!StepFactor::Cc.signed());
        assert!(!StepFactor::Heal.signed());
        assert!(StepFactor::Scarcity.signed());
        assert!(StepFactor::Saturation.signed());

        // from_name round-trip
        for f in StepFactor::iter() {
            assert_eq!(StepFactor::from_name(f.name()), Some(f));
        }
        assert_eq!(StepFactor::from_name("intent"), None); // intent is PlanFactor

        // PlanFactor: 3 variants
        assert_eq!(PlanFactor::count(), 3);
        let plan_names: Vec<_> = PlanFactor::iter().map(|f| f.name()).collect();
        assert_eq!(plan_names, ["intent", "tempo_gain", "self_survival"]);
        assert!(PlanFactor::Intent.signed());
        assert!(PlanFactor::TempoGain.signed());
        assert!(!PlanFactor::SelfSurvival.signed());

        for f in PlanFactor::iter() {
            assert_eq!(PlanFactor::from_name(f.name()), Some(f));
        }
        assert_eq!(PlanFactor::from_name("damage"), None); // damage is StepFactor

        // TerminalFactor: 8 variants
        assert_eq!(TerminalFactor::count(), 8);
        let term_names: Vec<_> = TerminalFactor::iter().map(|f| f.name()).collect();
        assert_eq!(
            term_names,
            [
                "exposure_at_end",
                "next_turn_lethality",
                "secure_kill",
                "ally_rescue",
                "board_control_gain",
                "line_actionability",
                "density_value",
                "pressure_spacing_zone",
            ]
        );
        // need_modulation spot checks
        assert_eq!(
            TerminalFactor::ExposureAtEnd.need_modulation(),
            crate::combat::ai::scoring::factors::registry::NeedAxis::SelfPreserve
        );
        assert_eq!(
            TerminalFactor::LineActionability.need_modulation(),
            crate::combat::ai::scoring::factors::registry::NeedAxis::None
        );

        for f in TerminalFactor::iter() {
            assert_eq!(TerminalFactor::from_name(f.name()), Some(f));
        }
    }

    // ── PlanFactorValues serde ─────────────────────────────────────────────────

    #[test]
    fn factor_values_serde_round_trip_named_map() {
        let mut pfv = PlanFactorValues::default();
        pfv.set(StepFactor::Damage, 1.2);
        pfv.set(StepFactor::KillNow, 0.8);
        pfv.set(StepFactor::KillPromised, 0.3);
        pfv.set(StepFactor::Cc, 0.5);
        pfv.set(StepFactor::Heal, 0.0);
        pfv.set(StepFactor::Scarcity, -0.2);
        pfv.set(StepFactor::Saturation, -0.4);
        pfv.set_plan(PlanFactor::Intent, 1.0);
        pfv.set_plan(PlanFactor::TempoGain, 0.6);
        pfv.set_plan(PlanFactor::SelfSurvival, 0.1);

        let json = serde_json::to_string(&pfv).unwrap();
        // Must be a named map, not a bare array.
        assert!(json.contains("\"damage\""), "expected named map: {json}");
        assert!(json.contains("\"self_survival\""), "missing key: {json}");

        let pfv2: PlanFactorValues = serde_json::from_str(&json).unwrap();
        assert_eq!(pfv, pfv2, "round-trip mismatch");
    }

    #[test]
    fn factor_values_missing_keys_default_zero() {
        let json = r#"{"damage": 0.5}"#;
        let pfv: PlanFactorValues = serde_json::from_str(json).unwrap();
        assert!((pfv.get(StepFactor::Damage) - 0.5).abs() < f32::EPSILON);
        assert_eq!(pfv.get(StepFactor::KillNow), 0.0);
        assert_eq!(pfv.get_plan(PlanFactor::Intent), 0.0);
    }

    #[test]
    fn factor_values_unknown_key_errors() {
        let json = r#"{"damage": 1.0, "nonexistent": 2.0}"#;
        let result: Result<PlanFactorValues, _> = serde_json::from_str(json);
        assert!(result.is_err(), "unknown key should produce an error");
    }
}
