//! `ScoreTrace` — typed log of score-affecting effects accumulated by pipeline stages.
//! See roadmap section "P3a" in docs/ai/restructure.md for migration context.
//!
//! Currently in P3a.0: types + `compute()` algebra + tests. No production stages
//! emit hits yet — they continue to mutate `ann.score` directly. Subsequent
//! P3a.{1..5} migrate stages one-by-one to push hits here.

use crate::combat::ai::adapt::EvaluationMode;

/// Source of a multiplier hit — for diagnostics only, not used in `compute()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiplierKind {
    Sanity,
    Critic,
}

#[derive(Debug, Clone, Copy)]
pub struct MultiplierHit {
    pub kind: MultiplierKind,
    pub value: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct AddendHit {
    /// Modifier name — corresponds to `ModifierContribution.name`
    /// (summon_bonus, trade_bonus, repair_bonus).
    pub name: &'static str,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskKind {
    /// Full poison mask: `compute()` returns `f32::NEG_INFINITY`.
    Poison,
}

#[derive(Debug, Clone, Copy)]
pub struct MaskHit {
    pub kind: MaskKind,
    /// Name of the source stage (for diagnostics).
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// Stage marks the plan as gated; pick_best sees the flag.
    Reject,
}

#[derive(Debug, Clone, Copy)]
pub struct GateHit {
    pub outcome: GateOutcome,
    pub source: &'static str,
}

/// Typed effect log for a single plan.
///
/// Canonical application order in `compute()` (see roadmap P3a):
/// 1. If any `Mask` with `Poison` → return `f32::NEG_INFINITY` (early exit).
/// 2. score = base
/// 3. score *= ∏ multipliers (in push order — sanity → critics)
/// 4. score += Σ addends (modifiers)
/// 5. If any `Gate` with `Reject` → plan stays in pool but gated flag is
///    returned separately from score.
#[derive(Debug, Clone, Default)]
pub struct ScoreTrace {
    pub base: f32,
    pub rescore_mode: Option<EvaluationMode>,
    pub multipliers: Vec<MultiplierHit>,
    pub addends: Vec<AddendHit>,
    pub masks: Vec<MaskHit>,
    pub gates: Vec<GateHit>,
}

impl ScoreTrace {
    /// Compute the final score from the effect log.
    /// See struct doc-comment for canonical order.
    pub fn compute(&self) -> f32 {
        if self.masks.iter().any(|m| matches!(m.kind, MaskKind::Poison)) {
            return f32::NEG_INFINITY;
        }
        let mut score = self.base;
        for m in &self.multipliers {
            score *= m.value;
        }
        for a in &self.addends {
            score += a.value;
        }
        score
    }

    /// `true` if any Gate has marked this plan as rejected.
    pub fn is_gated(&self) -> bool {
        self.gates.iter().any(|g| matches!(g.outcome, GateOutcome::Reject))
    }

    // Builder-style helpers — will be called by stages in P3a.{1..5}.
    pub fn push_multiplier(&mut self, hit: MultiplierHit) {
        self.multipliers.push(hit);
    }
    pub fn push_addend(&mut self, hit: AddendHit) {
        self.addends.push(hit);
    }
    pub fn push_mask(&mut self, hit: MaskHit) {
        self.masks.push(hit);
    }
    pub fn push_gate(&mut self, hit: GateHit) {
        self.gates.push(hit);
    }

    /// Clear accumulated effects (called by Finalize on rescore — P3a.5).
    /// Preserves `base` and `rescore_mode`.
    pub fn reset_effects(&mut self) {
        self.multipliers.clear();
        self.addends.clear();
        self.masks.clear();
        self.gates.clear();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_default_returns_zero() {
        let trace = ScoreTrace::default();
        assert_eq!(trace.compute(), 0.0);
    }

    #[test]
    fn compute_base_only() {
        let trace = ScoreTrace { base: 10.0, ..Default::default() };
        assert_eq!(trace.compute(), 10.0);
    }

    #[test]
    fn compute_applies_multipliers_in_push_order() {
        let mut trace = ScoreTrace { base: 10.0, ..Default::default() };
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Sanity, value: 0.5 });
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Critic, value: 0.8 });
        // 10 * 0.5 * 0.8 = 4.0
        assert!((trace.compute() - 4.0).abs() < 1e-6);
    }

    #[test]
    fn compute_applies_addends_after_multipliers() {
        let mut trace = ScoreTrace { base: 10.0, ..Default::default() };
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Sanity, value: 0.5 });
        trace.push_addend(AddendHit { name: "test_bonus", value: 2.0 });
        // (10 * 0.5) + 2 = 7.0 — NOT 10 * (0.5 + 2), critical semantic rule
        assert!((trace.compute() - 7.0).abs() < 1e-6);
    }

    #[test]
    fn compute_poison_mask_returns_neg_infinity() {
        let mut trace = ScoreTrace { base: 10.0, ..Default::default() };
        trace.push_mask(MaskHit { kind: MaskKind::Poison, source: "protect_self" });
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Sanity, value: 0.5 });
        trace.push_addend(AddendHit { name: "test_bonus", value: 5.0 });
        // Poison mask short-circuits all other effects
        assert_eq!(trace.compute(), f32::NEG_INFINITY);
    }

    #[test]
    fn compute_gates_do_not_zero_score() {
        let mut trace = ScoreTrace { base: 10.0, ..Default::default() };
        trace.push_gate(GateHit { outcome: GateOutcome::Reject, source: "killable_gate" });
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Critic, value: 0.5 });
        // Gate does not affect score — only sets is_gated flag
        assert!((trace.compute() - 5.0).abs() < 1e-6);
        assert!(trace.is_gated());
    }

    #[test]
    fn compute_addends_sum_in_order() {
        let mut trace = ScoreTrace::default(); // base = 0
        trace.push_addend(AddendHit { name: "a", value: 1.0 });
        trace.push_addend(AddendHit { name: "b", value: 2.0 });
        trace.push_addend(AddendHit { name: "c", value: 3.0 });
        assert!((trace.compute() - 6.0).abs() < 1e-6);
    }

    #[test]
    fn reset_effects_clears_but_preserves_base() {
        let mut trace = ScoreTrace { base: 10.0, ..Default::default() };
        trace.push_multiplier(MultiplierHit { kind: MultiplierKind::Sanity, value: 0.5 });
        trace.push_addend(AddendHit { name: "a", value: 2.0 });
        trace.push_mask(MaskHit { kind: MaskKind::Poison, source: "protect_self" });
        trace.push_gate(GateHit { outcome: GateOutcome::Reject, source: "killable_gate" });

        trace.reset_effects();

        assert_eq!(trace.base, 10.0, "base must be preserved after reset");
        assert!(trace.multipliers.is_empty());
        assert!(trace.addends.is_empty());
        assert!(trace.masks.is_empty());
        assert!(trace.gates.is_empty());
        // After reset, compute() == base
        assert_eq!(trace.compute(), 10.0);
    }
}
