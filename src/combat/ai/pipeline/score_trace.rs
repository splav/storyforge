//! `ScoreTrace` — typed log of score-affecting effects accumulated by pipeline stages.
//!
//! Runtime types (`ScoreTrace`, `AddendHit`, …) keep `&'static str` fields; the
//! `*Log` mirror types use owned `String` for JSONL serde (see the mirror section).

use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
use crate::combat::ai::pipeline::stages::sanity::SanityRule;

/// Source of a multiplier hit — for diagnostics only, not used in `compute()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiplierKind {
    Sanity,
    Critic,
}

/// Per-multiplier-hit diagnostic detail, derived by the drive-loop from the
/// paired `EffectObservation` (Sanity multiplier ↔ Sanity observation, Critic ↔
/// Critic). Carried in `score_trace_log` so mining/replay need no legacy fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MultiplierDetail {
    Sanity {
        rule: SanityRule,
    },
    Critic {
        critic: CriticKind,
        reason: CriticReason,
    },
}

/// Not `Copy`: `MultiplierDetail::Critic` carries `String`-bearing `CriticReason`.
#[derive(Debug, Clone)]
pub struct MultiplierHit {
    pub kind: MultiplierKind,
    pub value: f32,
    /// Required for `Sanity`/`Critic` kinds — `None` is invalid there and panics
    /// in debug builds (see `PlanAnnotation::apply_effect`).
    pub detail: Option<MultiplierDetail>,
}

#[derive(Debug, Clone, Copy)]
pub struct AddendHit {
    /// Modifier name — corresponds to `ModifierContribution.name`
    /// (summon_bonus, trade_bonus, repair_bonus).
    pub name: &'static str,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskKind {
    /// Full poison mask: `compute()` returns `f32::NEG_INFINITY`.
    Poison,
}

#[derive(Debug, Clone, Copy)]
pub struct MaskHit {
    pub kind: MaskKind,
    /// Name of the source stage (for diagnostics).
    pub source: &'static str,
    /// Score the plan would have had immediately before this mask applied.
    /// Drive-loop derives from the paired `Contract` observation when present.
    /// Mining/replay use this for "rejected score" diagnostic.
    pub original_score: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    /// Stage marks the plan as gated; pick_best sees the flag.
    Reject,
}

#[derive(Debug, Clone, Copy)]
pub struct GateHit {
    pub outcome: GateOutcome,
    pub source: &'static str,
}

// ── Serialisation mirror types ────────────────────────────────────────────────
//
// serde cannot deserialise the runtime types' `&'static str` fields, so these
// owned-`String` mirrors exist solely for JSONL. `ScoreTraceLog` is produced via
// `From<&ScoreTrace>` just before writing; the runtime path never reads it back.

/// Serialisable mirror of `MultiplierHit`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MultiplierHitLog {
    pub kind: MultiplierKind,
    pub value: f32,
    /// Diagnostic detail. Additive: older logs without this field deserialize as
    /// `None`, and mining falls back to legacy `ann.sanity` / `ann.critics`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<MultiplierDetail>,
}

/// Serialisable mirror of `AddendHit`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AddendHitLog {
    pub name: String,
    pub value: f32,
}

/// Serialisable mirror of `MaskHit`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MaskHitLog {
    pub kind: MaskKind,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_score: Option<f32>,
}

/// Serialisable mirror of `GateHit`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GateHitLog {
    pub outcome: GateOutcome,
    pub source: String,
}

/// Serialisable mirror of `ScoreTrace` for JSONL. The `skip_serializing_if`
/// guards omit empty vecs and `None` so logs stay compact.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ScoreTraceLog {
    #[serde(default)]
    pub base: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rescore_mode: Option<EvaluationMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multipliers: Vec<MultiplierHitLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addends: Vec<AddendHitLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<MaskHitLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateHitLog>,
}

impl From<&ScoreTrace> for ScoreTraceLog {
    fn from(t: &ScoreTrace) -> Self {
        Self {
            base: t.base,
            rescore_mode: t.rescore_mode,
            multipliers: t
                .multipliers
                .iter()
                .map(|h| MultiplierHitLog {
                    kind: h.kind,
                    value: h.value,
                    detail: h.detail.clone(),
                })
                .collect(),
            addends: t
                .addends
                .iter()
                .map(|h| AddendHitLog {
                    name: h.name.to_owned(),
                    value: h.value,
                })
                .collect(),
            masks: t
                .masks
                .iter()
                .map(|h| MaskHitLog {
                    kind: h.kind,
                    source: h.source.to_owned(),
                    original_score: h.original_score,
                })
                .collect(),
            gates: t
                .gates
                .iter()
                .map(|h| GateHitLog {
                    outcome: h.outcome,
                    source: h.source.to_owned(),
                })
                .collect(),
        }
    }
}

/// Typed effect log for a single plan.
///
/// `compute()` = `base × ∏ multipliers (push order: sanity → critics) + Σ addends`.
/// Masks and gates do NOT affect the numeric score — they surface via
/// `is_masked()` / `is_gated()` as separate selectability flags.
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
    /// `base × ∏multipliers + Σaddends`. **Always finite** — masks/gates are not
    /// applied here; selectability is via `is_masked()` / `is_gated()`.
    pub fn compute(&self) -> f32 {
        let mut score = self.base;
        for m in &self.multipliers {
            score *= m.value;
        }
        for a in &self.addends {
            score += a.value;
        }
        score
    }

    /// `true` if any Mask hit is present (regardless of kind).
    pub fn is_masked(&self) -> bool {
        !self.masks.is_empty()
    }

    /// `true` if any Gate has marked this plan as rejected.
    pub fn is_gated(&self) -> bool {
        self.gates
            .iter()
            .any(|g| matches!(g.outcome, GateOutcome::Reject))
    }

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
        let trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        assert_eq!(trace.compute(), 10.0);
    }

    #[test]
    fn compute_applies_multipliers_in_push_order() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: None,
        });
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Critic,
            value: 0.8,
            detail: None,
        });
        // 10 * 0.5 * 0.8 = 4.0
        assert!((trace.compute() - 4.0).abs() < 1e-6);
    }

    #[test]
    fn compute_applies_addends_after_multipliers() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: None,
        });
        trace.push_addend(AddendHit {
            name: "test_bonus",
            value: 2.0,
        });
        // (10 * 0.5) + 2 = 7.0 — NOT 10 * (0.5 + 2), critical semantic rule
        assert!((trace.compute() - 7.0).abs() < 1e-6);
    }

    #[test]
    fn compute_ignores_masks() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.push_mask(MaskHit {
            kind: MaskKind::Poison,
            source: "protect_self",
            original_score: None,
        });
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: None,
        });
        trace.push_addend(AddendHit {
            name: "test_bonus",
            value: 5.0,
        });
        // Mask does not affect compute() — score = 10.0 * 0.5 + 5.0 = 10.0
        assert!((trace.compute() - 10.0).abs() < 1e-6);
        // Mask is recorded in trace flags
        assert!(trace.is_masked(), "mask must be recorded in trace");
    }

    #[test]
    fn compute_gates_do_not_zero_score() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.push_gate(GateHit {
            outcome: GateOutcome::Reject,
            source: "killable_gate",
        });
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Critic,
            value: 0.5,
            detail: None,
        });
        // Gate does not affect score — only sets is_gated flag
        assert!((trace.compute() - 5.0).abs() < 1e-6);
        assert!(trace.is_gated());
    }

    #[test]
    fn compute_addends_sum_in_order() {
        let mut trace = ScoreTrace::default(); // base = 0
        trace.push_addend(AddendHit {
            name: "a",
            value: 1.0,
        });
        trace.push_addend(AddendHit {
            name: "b",
            value: 2.0,
        });
        trace.push_addend(AddendHit {
            name: "c",
            value: 3.0,
        });
        assert!((trace.compute() - 6.0).abs() < 1e-6);
    }

    #[test]
    fn reset_effects_clears_but_preserves_base() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: None,
        });
        trace.push_addend(AddendHit {
            name: "a",
            value: 2.0,
        });
        trace.push_mask(MaskHit {
            kind: MaskKind::Poison,
            source: "protect_self",
            original_score: None,
        });
        trace.push_gate(GateHit {
            outcome: GateOutcome::Reject,
            source: "killable_gate",
        });

        trace.reset_effects();

        assert_eq!(trace.base, 10.0, "base must be preserved after reset");
        assert!(trace.multipliers.is_empty());
        assert!(trace.addends.is_empty());
        assert!(trace.masks.is_empty());
        assert!(trace.gates.is_empty());
        // After reset, compute() == base
        assert_eq!(trace.compute(), 10.0);
    }

    // ── P3b: ScoreTraceLog serde tests ────────────────────────────────────────

    #[test]
    fn score_trace_log_roundtrips_through_json() {
        let mut trace = ScoreTrace {
            base: 10.0,
            ..Default::default()
        };
        trace.rescore_mode = Some(EvaluationMode::Default);
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: None,
        });
        trace.push_addend(AddendHit {
            name: "summon_bonus",
            value: 0.3,
        });
        trace.push_mask(MaskHit {
            kind: MaskKind::Poison,
            source: "protect_self",
            original_score: None,
        });
        trace.push_gate(GateHit {
            outcome: GateOutcome::Reject,
            source: "killable_gate",
        });

        let log = ScoreTraceLog::from(&trace);
        let json = serde_json::to_string(&log).expect("serialize");
        let restored: ScoreTraceLog = serde_json::from_str(&json).expect("deserialize");

        assert!((restored.base - 10.0).abs() < 1e-6);
        assert_eq!(restored.rescore_mode, Some(EvaluationMode::Default));
        assert_eq!(restored.multipliers.len(), 1);
        assert!(matches!(
            restored.multipliers[0].kind,
            MultiplierKind::Sanity
        ));
        assert!((restored.multipliers[0].value - 0.5).abs() < 1e-6);
        assert_eq!(restored.addends.len(), 1);
        assert_eq!(restored.addends[0].name, "summon_bonus");
        assert!((restored.addends[0].value - 0.3).abs() < 1e-6);
        assert_eq!(restored.masks.len(), 1);
        assert!(matches!(restored.masks[0].kind, MaskKind::Poison));
        assert_eq!(restored.masks[0].source, "protect_self");
        assert_eq!(restored.gates.len(), 1);
        assert!(matches!(restored.gates[0].outcome, GateOutcome::Reject));
        assert_eq!(restored.gates[0].source, "killable_gate");
    }

    #[test]
    fn score_trace_log_empty_fields_omitted_in_json() {
        // Empty vecs and None rescore_mode must not appear in JSON (skip_serializing_if).
        let log = ScoreTraceLog {
            base: 5.0,
            ..Default::default()
        };
        let json = serde_json::to_string(&log).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            v.get("multipliers").is_none(),
            "empty multipliers should be omitted"
        );
        assert!(
            v.get("addends").is_none(),
            "empty addends should be omitted"
        );
        assert!(v.get("masks").is_none(), "empty masks should be omitted");
        assert!(v.get("gates").is_none(), "empty gates should be omitted");
        assert!(
            v.get("rescore_mode").is_none(),
            "None rescore_mode should be omitted"
        );
        assert!((v["base"].as_f64().unwrap() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn score_trace_log_schema_additive_parses_without_field() {
        // A PlanAnnotation JSON without score_trace_log (simulating v32 corpus)
        // must deserialise successfully with score_trace_log == None.
        let json = r#"{"score": 1.5}"#;
        let ann: crate::combat::ai::outcome::PlanAnnotation =
            serde_json::from_str(json).expect("PlanAnnotation without score_trace_log must parse");
        assert!(
            ann.score_trace_log.is_none(),
            "score_trace_log must default to None when absent, got: {:?}",
            ann.score_trace_log,
        );
    }

    // ── Phase 3 Step 3: is_masked ─────────────────────────────────────────────

    #[test]
    fn is_masked_detects_any_mask() {
        let mut trace = ScoreTrace::default();
        assert!(!trace.is_masked());
        trace.push_mask(MaskHit {
            kind: MaskKind::Poison,
            source: "x",
            original_score: None,
        });
        assert!(trace.is_masked());
    }

    // ── TLE-1: enriched MultiplierHit / MaskHit detail tests ─────────────────

    #[test]
    fn tle_1_1_multiplier_hit_carries_sanity_detail_when_paired_with_sanity_obs() {
        use crate::combat::ai::pipeline::effects::{AppliedEffect, EffectObservation, ScoreHit};
        use crate::combat::ai::pipeline::order::StageId;
        use crate::combat::ai::pipeline::stages::sanity::{SanityHit, SanityRule};

        let mut ann = crate::combat::ai::outcome::PlanAnnotation::with_score(
            crate::combat::ai::outcome::PlanAnnotation::default(),
            1.0,
        );
        ann.score_trace.base = 1.0;
        ann.apply_effect(&AppliedEffect {
            source: StageId::Sanity,
            plan_index: 0,
            hit: ScoreHit::Multiplier(MultiplierHit {
                kind: MultiplierKind::Sanity,
                value: 0.5,
                detail: None,
            }),
            observability: Some(EffectObservation::Sanity(SanityHit {
                rule: SanityRule::HealerExposure,
                multiplier: 0.5,
            })),
        });

        assert_eq!(ann.score_trace.multipliers.len(), 1);
        assert_eq!(
            ann.score_trace.multipliers[0].detail,
            Some(MultiplierDetail::Sanity {
                rule: SanityRule::HealerExposure
            }),
            "detail must be derived from paired Sanity observation",
        );
    }

    #[test]
    fn tle_1_2_multiplier_hit_carries_critic_detail_when_paired_with_critic_obs() {
        use crate::combat::ai::pipeline::effects::{AppliedEffect, EffectObservation, ScoreHit};
        use crate::combat::ai::pipeline::order::StageId;
        use crate::combat::ai::pipeline::stages::critics::overcommit_into_danger::OvercommitSource;
        use crate::combat::ai::pipeline::stages::critics::{CriticHit, CriticKind, CriticReason};

        let mut ann = crate::combat::ai::outcome::PlanAnnotation::with_score(
            crate::combat::ai::outcome::PlanAnnotation::default(),
            1.0,
        );
        ann.score_trace.base = 1.0;
        let reason = CriticReason::OvercommitIntoDanger {
            source: OvercommitSource::SurvivalPath,
            ratio: 0.7,
        };
        ann.apply_effect(&AppliedEffect {
            source: StageId::Critics,
            plan_index: 0,
            hit: ScoreHit::Multiplier(MultiplierHit {
                kind: MultiplierKind::Critic,
                value: 0.6,
                detail: None,
            }),
            observability: Some(EffectObservation::Critic(CriticHit {
                critic: CriticKind::OvercommitIntoDanger,
                multiplier: 0.6,
                reason: reason.clone(),
            })),
        });

        assert_eq!(ann.score_trace.multipliers.len(), 1);
        assert_eq!(
            ann.score_trace.multipliers[0].detail,
            Some(MultiplierDetail::Critic {
                critic: CriticKind::OvercommitIntoDanger,
                reason,
            }),
            "detail must be derived from paired Critic observation",
        );
    }

    #[test]
    fn tle_1_3_mask_hit_carries_original_score_when_paired_with_contract_obs() {
        use crate::combat::ai::outcome::ContractMaskHit;
        use crate::combat::ai::pipeline::effects::{AppliedEffect, EffectObservation, ScoreHit};
        use crate::combat::ai::pipeline::order::StageId;

        let mut ann = crate::combat::ai::outcome::PlanAnnotation::with_score(
            crate::combat::ai::outcome::PlanAnnotation::default(),
            2.5,
        );
        ann.score_trace.base = 2.5;
        ann.apply_effect(&AppliedEffect {
            source: StageId::ProtectSelfMask,
            plan_index: 0,
            hit: ScoreHit::Mask(MaskHit {
                kind: MaskKind::Poison,
                source: "protect_self",
                original_score: None,
            }),
            observability: Some(EffectObservation::Contract(ContractMaskHit {
                mask: "protect_self".into(),
                original_score: 2.5,
            })),
        });

        assert_eq!(ann.score_trace.masks.len(), 1);
        assert_eq!(
            ann.score_trace.masks[0].original_score,
            Some(2.5),
            "original_score must be derived from paired Contract observation",
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "must carry detail")]
    fn tle_1_4_multiplier_hit_without_observation_panics_in_debug() {
        use crate::combat::ai::pipeline::effects::{AppliedEffect, ScoreHit};
        use crate::combat::ai::pipeline::order::StageId;

        let mut ann = crate::combat::ai::outcome::PlanAnnotation::with_score(
            crate::combat::ai::outcome::PlanAnnotation::default(),
            1.0,
        );
        ann.score_trace.base = 1.0;
        ann.apply_effect(&AppliedEffect {
            source: StageId::Sanity,
            plan_index: 0,
            hit: ScoreHit::Multiplier(MultiplierHit {
                kind: MultiplierKind::Sanity,
                value: 0.5,
                detail: None,
            }),
            observability: None,
        });
    }

    #[test]
    fn tle_1_5_score_trace_log_serializes_detail_field() {
        use crate::combat::ai::pipeline::stages::sanity::SanityRule;

        let mut trace = ScoreTrace {
            base: 1.0,
            ..Default::default()
        };
        trace.push_multiplier(MultiplierHit {
            kind: MultiplierKind::Sanity,
            value: 0.5,
            detail: Some(MultiplierDetail::Sanity {
                rule: SanityRule::HealerExposure,
            }),
        });

        // Round-trip through JSON.
        let log = ScoreTraceLog::from(&trace);
        let json = serde_json::to_string(&log).expect("serialize");
        let restored: ScoreTraceLog = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.multipliers.len(), 1);
        assert_eq!(
            restored.multipliers[0].detail,
            Some(MultiplierDetail::Sanity {
                rule: SanityRule::HealerExposure
            }),
            "detail must round-trip through JSON",
        );

        // Verify JSON shape contains expected keys.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let detail = &v["multipliers"][0]["detail"];
        assert_eq!(
            detail["kind"], "sanity",
            "detail kind must be snake_case 'sanity'"
        );
        assert_eq!(
            detail["rule"], "healer_exposure",
            "rule must be snake_case serialized"
        );

        // v33-shape JSON (no detail field) deserializes with detail=None.
        let old_json = r#"{"base":1.0,"multipliers":[{"kind":"sanity","value":0.5}]}"#;
        let old_log: ScoreTraceLog = serde_json::from_str(old_json).expect("deserialize old");
        assert_eq!(
            old_log.multipliers[0].detail, None,
            "old logs without detail field must deserialize as None",
        );
    }
}
