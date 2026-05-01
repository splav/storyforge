//! Adaptation layer — data types and algorithm.
//!
//! Pipeline position: between `sanity_adjust_plans` (plan-level cost
//! correction, soft multipliers) and contract masks (intent↔plan coherence
//! enforcement). Adaptation answers the question:
//!
//! > "Facts discovered after measurement make the current value function
//! >  inadequate for some plans. Which plans, and what's the right
//! >  evaluation regime for them instead?"
//!
//! Example: `expected_aoo_damage >= actor_hp` for a plan means the actor
//! does not continue to exist after this turn — `continue-to-exist value =
//! 0` — so scoring the plan under `FocusTarget`/`ApplyCC`/... is semantically
//! wrong. The only evaluation regime that stays meaningful is **LastStand**:
//! "what useful thing do I achieve before going down".
//!
//! # Invariants
//!
//! The layer is intentionally narrow. These are load-bearing:
//!
//! 1. **ONE PASS.** `apply_adaptation` runs once per `pick_action`, after
//!    sanity, before contract masks. No internal loops, no re-entry.
//! 2. **FACTS ONLY.** Triggers are snapshot facts
//!    (`expected_aoo_damage >= hp`, `plan_is_defensive`, `global_intent`).
//!    Never post-score comparisons — that would create circular meaning.
//! 3. **NO PENALTIES / NO MASKS.** The layer only maps
//!    `(plan → EvaluationMode)` and triggers intent-column rescore for the
//!    affected rows. It does not multiply scores and does not write `-∞`.
//!    That territory belongs to sanity (multipliers) and contract (masks).
//! 4. **IDEMPOTENT.** Applying adaptation a second time is a no-op.
//!    `EvaluationMode` changes at most once per plan.
//! 5. **CONTRACT-NEUTRAL.** Adaptation does not know about contract masks.
//!    Contract runs AFTER adaptation and masks only plans with
//!    `mode = Default` — plans with `mode != Default` have already opted
//!    out of the original intent's contract by virtue of the regime switch.
//!
//! Adding a new `AdaptationReason`: only if the new case satisfies all five
//! invariants. A "I want to penalise X a bit more" rule belongs in sanity,
//! not here.
//!
//! # Layout
//!
//! - `mod.rs` (this file) — data types: `EvaluationMode`, `AdaptationReason`,
//!   `Adaptation`.
//! - `select.rs` — algorithm: `select_evaluation_modes` (pure),
//!   `apply_adaptation` (mut), helpers `pending_dot_before_next_action`,
//!   `plan_has_self_rescue`.

pub mod select;

pub use select::{apply_adaptation, pending_dot_before_next_action, select_evaluation_modes};

use crate::combat::ai::intent::TacticalIntent;

/// Evaluation regime used when scoring the intent-column of a plan.
///
/// `Default` = score under the global `TacticalIntent` selected by
/// `select_intent`. `LastStand` = score as if the actor is committed to a
/// "final useful action" — the `TacticalIntent::LastStand` scoring table in
/// `intent_score()` is reused so no new scoring code is needed; this enum
/// only selects *which* existing table to apply, per plan.
///
/// Populated by `apply_adaptation`; consumed by the scorer's per-plan
/// intent rescore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    /// Score under the global tactical intent.
    #[default]
    Default,
    /// Score under the LastStand regime — "final useful action" weighting.
    /// Used when the plan either kills the actor (per-plan) or the global
    /// intent cannot be satisfied (global ProtectSelf → no defensive).
    LastStand,
}

impl EvaluationMode {
    /// Returns the effective intent to use for scoring this plan's
    /// intent-column. `Default` defers to the caller's global intent;
    /// `LastStand` always overrides to `TacticalIntent::LastStand` regardless
    /// of what the caller passes.
    ///
    /// Consolidates the "which intent drives scoring?" decision in one
    /// place so callers don't have to know the mapping.
    pub fn effective_intent(self, global: TacticalIntent) -> TacticalIntent {
        match self {
            EvaluationMode::Default => global,
            EvaluationMode::LastStand => TacticalIntent::LastStand,
        }
    }
}

/// Fact-based reason an individual plan's evaluation regime was switched.
/// Carries enough numeric context for debug/log to explain the switch —
/// no post-score values, only snapshot facts (see invariant #2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdaptationReason {
    /// Plan's expected AoO damage on its move transitions reaches or
    /// exceeds the actor's current HP → continue-to-exist value = 0 →
    /// evaluate under LastStand. Per-plan override.
    ///
    /// **Horizon — step-local.** AoO fires *during* a specific Move
    /// transition; the suffix of the plan doesn't help if the actor
    /// dies mid-path. So `expected_aoo_damage(plan)` sums per-step AoO
    /// bleed on transitions, not an end-of-turn projection.
    ///
    /// "Expected" because `expected_aoo_damage` is an EV aggregate
    /// (crit-fail is disabled in sim); in a live turn the plan may or
    /// may not kill the actor. The adaptation threshold is conservative:
    /// if EV says ≥ HP, treat it as self-terminating.
    ExpectedSelfLethal { aoo_dmg: f32, actor_hp: i32 },
    /// Global intent is `ProtectSelf` but **no** plan in the pool is
    /// defensive (by `plan_is_defensive`). The ProtectSelf contract
    /// cannot be satisfied *spatially*, so every plan is evaluated
    /// under LastStand. Global override (applied to all plans).
    ProtectSelfNoDefensive,
    /// Global intent is `ProtectSelf`, defensive options exist, but
    /// pending DoT (`sum(dot_per_tick + hp_percent_dot) over active
    /// statuses`) exceeds `actor.hp` AND no plan in the pool would
    /// leave the actor alive at end-of-turn. Contract is *temporally*
    /// unsatisfiable: the actor can get to safety but still dies from
    /// DoT before acting again.
    ///
    /// **Horizon — end-of-turn.** In this engine only the current actor
    /// mutates state during his own turn; DoT ticks fire on the
    /// applier's turn, *after* this actor finishes. So the correct
    /// rescue horizon is `sim_snapshots.last()` — "will I be alive when
    /// my turn ends" — not the committed prefix.
    ///
    /// Global override (applied to all plans). Payload:
    /// `pending_dot` = `pending_dot_before_next_action(active)`,
    /// `actor_hp` = `active.hp`.
    ProtectSelfFutile { pending_dot: i32, actor_hp: i32 },
}

impl AdaptationReason {
    /// Stable snake_case code for analyzers / JSONL `adaptation_reason`
    /// field. Keep in sync with schema_version in `log.rs` when renaming.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
            Self::ProtectSelfFutile { .. } => "protect_self_futile",
        }
    }
}

/// Output of the adaptation pass. Parallel vectors aligned with the plan
/// pool: `modes[i]` is the evaluation regime for `plans[i]`, and
/// `reasons[i]` is `Some(_)` iff `modes[i] != Default`.
///
/// Consumed by (a) `pick_action` when wrapping the committed plan's
/// `IntentReason` as `Adapted { prior, reason }`, and (b) the contract
/// mask (`apply_protect_self_mask`) to skip plans that opted out of the
/// current intent's contract via a mode switch.
pub struct Adaptation {
    pub modes: Vec<EvaluationMode>,
    pub reasons: Vec<Option<AdaptationReason>>,
}

impl Adaptation {
    /// Empty adaptation for a pool of size `n` — every plan at Default,
    /// no reasons recorded. Used as the initial state before
    /// `apply_adaptation` runs, and as a safe fallback in tests.
    pub fn empty(n: usize) -> Self {
        Self {
            modes: vec![EvaluationMode::Default; n],
            reasons: vec![None; n],
        }
    }

    /// Did any plan end up in a non-Default mode?
    pub fn any_adapted(&self) -> bool {
        self.modes.iter().any(|m| !matches!(m, EvaluationMode::Default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── effective_intent ─────────────────────────────────────────────────

    #[test]
    fn default_mode_defers_to_global_intent() {
        let global = TacticalIntent::Reposition;
        let got = EvaluationMode::Default.effective_intent(global);
        assert!(matches!(got, TacticalIntent::Reposition));
    }

    #[test]
    fn last_stand_mode_overrides_global() {
        // Even if the caller passes something unrelated, LastStand pins the
        // scoring regime — this is the whole point of the per-plan override.
        let global = TacticalIntent::Reposition;
        let got = EvaluationMode::LastStand.effective_intent(global);
        assert!(matches!(got, TacticalIntent::LastStand));
    }
}
