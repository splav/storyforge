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
//! # Invariants (load-bearing)
//!
//! 1. **ONE PASS.** `apply_adaptation` runs once per `pick_action`, after
//!    sanity, before contract masks. No re-entry.
//! 2. **FACTS ONLY.** Triggers are snapshot facts, never post-score
//!    comparisons — that would create circular meaning.
//! 3. **NO PENALTIES / NO MASKS.** Only maps `plan → EvaluationMode` and
//!    triggers intent-column rescore. No score multiply, no `-∞` — that's
//!    sanity (multipliers) and contract (masks).
//! 4. **IDEMPOTENT.** `EvaluationMode` changes at most once per plan.
//! 5. **CONTRACT-NEUTRAL.** Contract runs after and masks only `mode =
//!    Default` plans; `mode != Default` has already opted out of the
//!    original intent's contract via the regime switch.
//!
//! A new `AdaptationReason` must satisfy all five. "Penalise X a bit more"
//! belongs in sanity, not here.
//!
//! # Layout
//!
//! - `mod.rs` (this file) — data types: `EvaluationMode`, `AdaptationReason`,
//!   `Adaptation`.
//! - `select.rs` — algorithm: `select_evaluation_modes` (pure),
//!   `apply_adaptation` (mut), helpers `pending_dot_before_next_action`,
//!   `plan_has_self_rescue`.

pub mod select;

pub(crate) use select::plan_has_lethal_transit;
pub use select::{apply_adaptation, pending_dot_before_next_action, select_evaluation_modes};

/// Evaluation regime used when scoring the intent-column of a plan.
/// Per-variant semantics below. Populated by `apply_adaptation`; consumed by
/// the scorer's per-plan intent rescore (passed as `mode` to `intent_score`).
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
    /// Score under the Flee regime — maximise distance from nearest enemy.
    /// Offensive casts score lowest (suppressed); self-heal/self-buff allowed.
    /// Applied when a boss phase sets `ai_behavior = "flee"` on the unit.
    Flee,
}

/// Fact-based reason an individual plan's evaluation regime was switched.
/// Carries enough numeric context for debug/log to explain the switch —
/// no post-score values, only snapshot facts (see invariant #2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdaptationReason {
    /// Plan's expected move-transition AoO damage ≥ actor HP →
    /// continue-to-exist value = 0 → LastStand. Per-plan override.
    ///
    /// **Horizon — step-local.** AoO fires *during* a Move transition, so
    /// `expected_aoo_damage` sums per-step bleed, not an end-of-turn
    /// projection. "Expected" = EV aggregate (crit-fail disabled in sim);
    /// the EV-≥-HP threshold is conservative.
    ExpectedSelfLethal { aoo_dmg: f32, actor_hp: i32 },
    /// Global intent is `ProtectSelf` but **no** plan in the pool is
    /// defensive (by `plan_is_defensive`). The ProtectSelf contract
    /// cannot be satisfied *spatially*, so every plan is evaluated
    /// under LastStand. Global override (applied to all plans).
    ProtectSelfNoDefensive,
    /// `ProtectSelf`, defensive options exist, but pending DoT exceeds
    /// `actor.hp` AND no plan leaves the actor alive at end-of-turn.
    /// Contract is *temporally* unsatisfiable: reachable safety, still dies
    /// from DoT before acting again.
    ///
    /// **Horizon — end-of-turn.** DoT ticks fire on the applier's turn,
    /// after this actor finishes, so the rescue horizon is
    /// `sim_snapshots.last()`, not the committed prefix.
    ///
    /// Global override. Payload: `pending_dot =
    /// pending_dot_before_next_action(active)`, `actor_hp = active.hp`.
    ProtectSelfFutile { pending_dot: i32, actor_hp: i32 },
    /// A unit-level fact override imposed by a content phase transition
    /// (`ai_behavior` field in PhaseDef). Highest precedence — short-circuits
    /// all other adaptation rules. FACTS-ONLY, IDEMPOTENT.
    ///
    /// `mode` is the regime that was forced. Named "Forced" (not "Fleeing")
    /// so future regimes (Patrol, Panic, …) can reuse this variant.
    Forced { mode: EvaluationMode },
}

impl AdaptationReason {
    /// Stable snake_case code for analyzers / JSONL `adaptation_reason`
    /// field. Keep in sync with schema_version in `log.rs` when renaming.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
            Self::ProtectSelfFutile { .. } => "protect_self_futile",
            Self::Forced { .. } => "forced",
        }
    }
}

impl EvaluationMode {
    /// Serde default for `AdaptationData.mode` field: pre-Flee logs had only
    /// LastStand adaptations, so defaulting to LastStand is safe for old data.
    pub fn default_last_stand() -> Self {
        Self::LastStand
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
        self.modes
            .iter()
            .any(|m| !matches!(m, EvaluationMode::Default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Adaptation helpers ────────────────────────────────────────────────

    #[test]
    fn adaptation_empty_is_all_default() {
        let a = Adaptation::empty(3);
        assert_eq!(a.modes.len(), 3);
        assert!(a.modes.iter().all(|m| *m == EvaluationMode::Default));
        assert!(a.reasons.iter().all(|r| r.is_none()));
        assert!(!a.any_adapted());
    }

    #[test]
    fn any_adapted_true_when_last_stand_present() {
        let mut a = Adaptation::empty(2);
        a.modes[1] = EvaluationMode::LastStand;
        assert!(a.any_adapted());
    }
}
