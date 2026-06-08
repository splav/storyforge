//! StageSpec — typed read/write contracts for the production pipeline.
//!
//! See roadmap section "P2" in docs/ai/restructure.md for design rationale:
//! - Why `Score` is split into 3 fields (ScoreBase / ScoreEffects / FinalScore).
//! - Why `Gate` is split into Pre/Post (Viability before Finalize vs KillableGate after).
//! - Why Multiplier and Addend are separate ScoreEffect variants.
//! - What belongs in INITIAL_FIELDS (populated before the first stage runs).
//!
//! # Design choice: separate STAGE_SPECS table
//!
//! `StageSpec` lives in a parallel `STAGE_SPECS: &[StageSpec]` table rather than
//! inside `StageEntry`. Rationale:
//! - Spec is logically independent of pipeline split (PRE_MASK / POST_MASK share
//!   the same spec, so duplicating spec data into three constants is noise).
//! - `StageEntry` stays `const`-constructible with simple fn-pointers; adding
//!   a `spec: StageSpec` field with slice references would require `&'static` and
//!   careful const initialisation for every existing table.
//! - Validator operates on a `&[StageSpec]` slice and doesn't need `apply` ptrs.
//!
//! Invariant: `STAGE_SPECS.len() == PRODUCTION_PIPELINE.len()` and both are
//! ordered identically.  Enforced by the `stage_specs_length_matches_pipeline`
//! test below and by `validate_pipeline(STAGE_SPECS)` in
//! `production_pipeline_order_is_valid`.

use crate::combat::ai::pipeline::order::StageId;

// ── AnnotationField ───────────────────────────────────────────────────────────

/// Coarse semantic fields of `PlanAnnotation` tracked by the validator.
///
/// Not a 1-to-1 mapping to `PlanAnnotation` struct fields — groups related
/// fields into logical buckets (e.g. `PerItem` covers `per_item`,
/// `reject_reasons_per_item`, and `considerations_per_item`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationField {
    /// `ann.factors` (raw factor decomposition from `score_plans_with_raw`).
    RawFactors,
    /// `ann.outcomes` (per-step `ActionOutcomeEstimate` from sim).
    Outcomes,
    /// Plan steps and `final_pos` (`TurnPlan` content itself).
    Plan,
    /// `BattleSnapshot`, influence maps, unit positions — world input view.
    SnapshotFacts,
    /// `ann.score_initial` — base score from the initial scoring pass,
    /// stored in annotation before any pipeline stage runs.
    InitialScoreFacts,
    /// `ann.score` after `FinalizeStage` (Rescore) — mode-aware baseline.
    ScoreBase,
    /// `ann.score` after multipliers, addends, and masks are applied
    /// (Sanity → Critics → ProtectSelfMask → PlanModifiers chain).
    ScoreEffects,
    /// Final resolved score read by `PickBestStage`.
    FinalScore,
    /// `ann.repair_affinity` — populated by `RepairAffinityStage`.
    RepairAffinity,
    /// `ann.per_item`, `ann.reject_reasons_per_item`,
    /// `ann.considerations_per_item` — per-agenda-item data.
    PerItem,
    /// `ann.viability` — gate result; also `reject_reasons_per_item` at pick time.
    Eligibility,
    /// `ann.adaptation` — `EvaluationMode` + reason, set by `ModeSelectionStage`.
    EvaluationMode,
}

// ── ScoreEffect ───────────────────────────────────────────────────────────────

/// The kind of effect a stage has on scores.
///
/// Drives two validator invariants:
/// 1. Exactly one `Rescore` stage.
/// 2. `Rescore` cannot follow any `Multiplier | Addend | Mask | PostScoreGate`.
/// 3. Every `PostScoreGate` must follow `Rescore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreEffect {
    /// Filters/marks plans *before* Finalize; does not touch `ann.score`.
    /// Example: `ViabilityStage` (may rescore via `rescore_with_intent` when
    /// the gate fails, but from the pipeline-validator perspective it is a
    /// pre-score gate that runs before `Finalize` establishes `ScoreBase`).
    PreScoreGate,
    /// Establishes `ScoreBase` — the mode-aware baseline score.
    /// Example: `FinalizeStage`.
    Rescore,
    /// Multiplies `ann.score` in-place.
    /// Examples: `SanityStage`, `CriticsStage`.
    Multiplier,
    /// Adds a signed delta to `ann.score`.
    /// Example: `PlanModifiersStage`.
    Addend,
    /// Sets `ann.score = NEG_INFINITY` for masked plans.
    /// Example: `ProtectSelfMaskStage`.
    Mask,
    /// Applies after all score effects; gates plans without resetting score.
    /// Example: `KillableGateStage`.
    PostScoreGate,
}

// ── StageSpec ─────────────────────────────────────────────────────────────────

/// Typed read/write contract for one pipeline stage.
pub struct StageSpec {
    pub id: StageId,
    pub reads: &'static [AnnotationField],
    pub writes: &'static [AnnotationField],
    pub score_effect: Option<ScoreEffect>,
}

// ── INITIAL_FIELDS ────────────────────────────────────────────────────────────

/// Fields populated *before* the first pipeline stage runs.
///
/// The validator treats these as always-available writers so that stages
/// depending on them pass the reads-before-writes check.
pub const INITIAL_FIELDS: &[AnnotationField] = &[
    AnnotationField::RawFactors,        // populated by score_plans_with_raw
    AnnotationField::Outcomes,          // populated by sim::apply_step + outcome builder
    AnnotationField::Plan,              // TurnPlan steps + final_pos
    AnnotationField::SnapshotFacts,     // BattleSnapshot, influence maps
    AnnotationField::InitialScoreFacts, // ann.score_initial from initial scoring pass
];

// ── STAGE_SPECS ───────────────────────────────────────────────────────────────

/// Spec table for all 12 production stages, in the same order as
/// `PRODUCTION_PIPELINE`.
///
/// Length must equal `PRODUCTION_PIPELINE.len()` — enforced by test.
pub const STAGE_SPECS: &[StageSpec] = &[
    // 0 — Viability
    StageSpec {
        id: StageId::Viability,
        reads: &[
            AnnotationField::SnapshotFacts,
            AnnotationField::RawFactors,
            AnnotationField::Outcomes,
            AnnotationField::Plan,
        ],
        writes: &[AnnotationField::Eligibility],
        score_effect: Some(ScoreEffect::PreScoreGate),
    },
    // 1 — ItemScoring
    StageSpec {
        id: StageId::ItemScoring,
        reads: &[
            AnnotationField::SnapshotFacts,
            AnnotationField::Outcomes,
            AnnotationField::Plan,
            AnnotationField::RawFactors,
            AnnotationField::Eligibility,
        ],
        writes: &[AnnotationField::PerItem],
        score_effect: None,
    },
    // 2 — ModeSelection
    StageSpec {
        id: StageId::ModeSelection,
        reads: &[
            AnnotationField::SnapshotFacts,
            AnnotationField::Outcomes,
            AnnotationField::RawFactors,
            AnnotationField::Plan,
        ],
        writes: &[AnnotationField::EvaluationMode],
        score_effect: None,
    },
    // 3 — Finalize  (Rescore: establishes ScoreBase)
    StageSpec {
        id: StageId::Finalize,
        reads: &[
            AnnotationField::EvaluationMode,
            AnnotationField::RawFactors,
            AnnotationField::InitialScoreFacts,
        ],
        writes: &[AnnotationField::ScoreBase],
        score_effect: Some(ScoreEffect::Rescore),
    },
    // 4 — Sanity  (Multiplier: multiplies ScoreBase → ScoreEffects)
    StageSpec {
        id: StageId::Sanity,
        reads: &[
            AnnotationField::SnapshotFacts,
            AnnotationField::ScoreBase,
            AnnotationField::EvaluationMode,
        ],
        writes: &[AnnotationField::ScoreEffects],
        score_effect: Some(ScoreEffect::Multiplier),
    },
    // 5 — Critics  (Multiplier: multiplies on top of Sanity)
    StageSpec {
        id: StageId::Critics,
        reads: &[
            AnnotationField::Outcomes,
            AnnotationField::SnapshotFacts,
            AnnotationField::ScoreEffects,
        ],
        writes: &[AnnotationField::ScoreEffects],
        score_effect: Some(ScoreEffect::Multiplier),
    },
    // 6 — ProtectSelfMask  (Mask: sets score = NEG_INFINITY for non-defensive plans)
    StageSpec {
        id: StageId::ProtectSelfMask,
        reads: &[
            AnnotationField::SnapshotFacts,
            AnnotationField::EvaluationMode,
            AnnotationField::ScoreEffects,
        ],
        writes: &[AnnotationField::ScoreEffects],
        score_effect: Some(ScoreEffect::Mask),
    },
    // 7 — KillableGate  (PostScoreGate: gates after all score effects are applied)
    StageSpec {
        id: StageId::KillableGate,
        reads: &[
            AnnotationField::Outcomes,
            AnnotationField::SnapshotFacts,
            AnnotationField::ScoreEffects,
        ],
        writes: &[AnnotationField::ScoreEffects],
        score_effect: Some(ScoreEffect::PostScoreGate),
    },
    // 8 — RepairAffinity  (no score effect)
    StageSpec {
        id: StageId::RepairAffinity,
        reads: &[AnnotationField::SnapshotFacts, AnnotationField::Outcomes],
        writes: &[AnnotationField::RepairAffinity],
        score_effect: None,
    },
    // 9 — OverlayConsiderations  (no score effect)
    StageSpec {
        id: StageId::OverlayConsiderations,
        reads: &[
            AnnotationField::Outcomes,
            AnnotationField::SnapshotFacts,
            AnnotationField::PerItem,
            AnnotationField::EvaluationMode,
            AnnotationField::RepairAffinity,
        ],
        writes: &[AnnotationField::PerItem],
        score_effect: None,
    },
    // 10 — PlanModifiers  (Addend: adds signed delta to score)
    StageSpec {
        id: StageId::PlanModifiers,
        reads: &[
            AnnotationField::RepairAffinity,
            AnnotationField::Outcomes,
            AnnotationField::ScoreEffects,
        ],
        writes: &[AnnotationField::ScoreEffects],
        score_effect: Some(ScoreEffect::Addend),
    },
    // 11 — PickBest  (no score effect; reads final score effects to pick winner)
    StageSpec {
        id: StageId::PickBest,
        reads: &[
            AnnotationField::ScoreEffects,
            AnnotationField::PerItem,
            AnnotationField::Eligibility,
            AnnotationField::InitialScoreFacts,
        ],
        writes: &[],
        score_effect: None,
    },
];

// ── ValidationError ───────────────────────────────────────────────────────────

/// Errors reported by `validate_pipeline`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A stage reads a field that is neither in `INITIAL_FIELDS` nor written by
    /// any earlier stage.
    MissingWriter {
        stage: StageId,
        field: AnnotationField,
    },

    /// The pipeline contains no `Rescore` stage.
    NoRescore,

    /// The pipeline contains more than one `Rescore` stage.
    MultipleRescore,

    /// A `Rescore` stage appears *after* a stage that already applied score
    /// effects (`Multiplier | Addend | Mask | PostScoreGate`), which would
    /// overwrite those effects.
    RescoreAfterEffect {
        rescore_stage: StageId,
        effect_stage: StageId,
    },

    /// A `PostScoreGate` stage appears before the `Rescore` stage, meaning it
    /// would gate on a score that has not yet been established.
    PostScoreGateBeforeRescore { gate: StageId, rescore: StageId },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::MissingWriter { stage, field } =>
                write!(f, "stage {stage:?} reads {field:?} but no earlier stage writes it (and it is not in INITIAL_FIELDS)"),
            ValidationError::NoRescore =>
                write!(f, "pipeline has no Rescore stage (required exactly once)"),
            ValidationError::MultipleRescore =>
                write!(f, "pipeline has more than one Rescore stage (required exactly once)"),
            ValidationError::RescoreAfterEffect { rescore_stage, effect_stage } =>
                write!(f, "Rescore stage {rescore_stage:?} appears after effect stage {effect_stage:?} — would overwrite applied score effects"),
            ValidationError::PostScoreGateBeforeRescore { gate, rescore } =>
                write!(f, "PostScoreGate stage {gate:?} appears before Rescore stage {rescore:?} — cannot gate on a score not yet established"),
        }
    }
}

// ── validate_pipeline ─────────────────────────────────────────────────────────

/// Validate a pipeline spec slice for ordering invariants.
///
/// # Invariants checked
///
/// 1. **reads-writes**: every field read by stage `i` must be in
///    `INITIAL_FIELDS` or written by some stage `j < i`.
///
/// 2. **score_effect ordering**:
///    - Exactly one `Rescore` stage.
///    - `Rescore` must not follow any `Multiplier | Addend | Mask | PostScoreGate`.
///    - Every `PostScoreGate` must follow `Rescore`.
///
/// `PreScoreGate` before `Rescore` is explicitly allowed (e.g. `ViabilityStage`).
pub fn validate_pipeline(specs: &[StageSpec]) -> Result<(), ValidationError> {
    // ── Invariant 1: reads-writes ─────────────────────────────────────────────
    let mut available: Vec<AnnotationField> = INITIAL_FIELDS.to_vec();

    for spec in specs {
        for &field in spec.reads {
            if !available.contains(&field) {
                return Err(ValidationError::MissingWriter {
                    stage: spec.id,
                    field,
                });
            }
        }
        for &field in spec.writes {
            if !available.contains(&field) {
                available.push(field);
            }
        }
    }

    // ── Invariant 2: score_effect ordering ───────────────────────────────────

    // Count and locate Rescore stages.
    let rescore_count = specs
        .iter()
        .filter(|s| s.score_effect == Some(ScoreEffect::Rescore))
        .count();

    if rescore_count == 0 {
        return Err(ValidationError::NoRescore);
    }
    if rescore_count > 1 {
        return Err(ValidationError::MultipleRescore);
    }

    let rescore_idx = specs
        .iter()
        .position(|s| s.score_effect == Some(ScoreEffect::Rescore))
        .unwrap(); // safe: count == 1

    // Check: every PostScoreGate must follow Rescore (checked first — this is
    // more specific than RescoreAfterEffect and takes priority when both would fire).
    for (idx, spec) in specs.iter().enumerate() {
        if spec.score_effect == Some(ScoreEffect::PostScoreGate) && idx < rescore_idx {
            return Err(ValidationError::PostScoreGateBeforeRescore {
                gate: spec.id,
                rescore: specs[rescore_idx].id,
            });
        }
    }

    // Effects that, if applied before Rescore, would make the Rescore invalid.
    // PostScoreGate is excluded here: it is checked above (before Rescore = its
    // own error). PostScoreGate appearing after Rescore is correct and allowed.
    const ILLEGAL_BEFORE_RESCORE: &[ScoreEffect] = &[
        ScoreEffect::Multiplier,
        ScoreEffect::Addend,
        ScoreEffect::Mask,
    ];

    // Check: Rescore must not follow any Multiplier / Addend / Mask stage.
    for (idx, spec) in specs.iter().enumerate() {
        if idx >= rescore_idx {
            break;
        }
        if let Some(effect) = spec.score_effect {
            if ILLEGAL_BEFORE_RESCORE.contains(&effect) {
                return Err(ValidationError::RescoreAfterEffect {
                    rescore_stage: specs[rescore_idx].id,
                    effect_stage: spec.id,
                });
            }
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::order::PRODUCTION_PIPELINE;

    /// Production pipeline has 12 stages; STAGE_SPECS must have the same length.
    #[test]
    fn stage_specs_length_matches_pipeline() {
        assert_eq!(
            STAGE_SPECS.len(),
            PRODUCTION_PIPELINE.len(),
            "STAGE_SPECS and PRODUCTION_PIPELINE must have the same length"
        );
    }

    /// STAGE_SPECS IDs match PRODUCTION_PIPELINE IDs in the same order.
    #[test]
    fn stage_specs_ids_match_pipeline_order() {
        for (spec, entry) in STAGE_SPECS.iter().zip(PRODUCTION_PIPELINE.iter()) {
            assert_eq!(
                spec.id, entry.id,
                "STAGE_SPECS[n].id must equal PRODUCTION_PIPELINE[n].id"
            );
        }
    }

    /// The production pipeline spec must pass validation.
    #[test]
    fn production_pipeline_order_is_valid() {
        validate_pipeline(STAGE_SPECS).expect("production pipeline spec must be valid");
    }

    // ── Negative tests ────────────────────────────────────────────────────────

    /// Negative: a stage reads a field that no earlier stage writes.
    #[test]
    fn negative_reads_without_writer() {
        // Put RepairAffinity-reader (OverlayConsiderations) before RepairAffinity-writer.
        let overlay_spec = StageSpec {
            id: StageId::OverlayConsiderations,
            reads: &[AnnotationField::RepairAffinity],
            writes: &[AnnotationField::PerItem],
            score_effect: None,
        };
        let repair_spec = StageSpec {
            id: StageId::RepairAffinity,
            reads: &[AnnotationField::SnapshotFacts],
            writes: &[AnnotationField::RepairAffinity],
            score_effect: None,
        };
        // Finalize (Rescore) must be present; put it before so ordering rules pass.
        let finalize_spec = StageSpec {
            id: StageId::Finalize,
            reads: &[AnnotationField::RawFactors],
            writes: &[AnnotationField::ScoreBase],
            score_effect: Some(ScoreEffect::Rescore),
        };
        // overlay reads RepairAffinity before repair writes it → MissingWriter.
        let bad_pipeline = [overlay_spec, repair_spec, finalize_spec];
        let err = validate_pipeline(&bad_pipeline).unwrap_err();
        assert_eq!(
            err,
            ValidationError::MissingWriter {
                stage: StageId::OverlayConsiderations,
                field: AnnotationField::RepairAffinity,
            }
        );
    }

    /// Negative: Rescore stage appears after a Multiplier.
    #[test]
    fn negative_rescore_after_multiplier() {
        // Sanity reads only INITIAL_FIELDS so the reads-writes check passes.
        // The ordering violation (Multiplier before Rescore) is what we test.
        let sanity_spec = StageSpec {
            id: StageId::Sanity,
            reads: &[AnnotationField::SnapshotFacts],
            writes: &[AnnotationField::ScoreEffects],
            score_effect: Some(ScoreEffect::Multiplier),
        };
        // Finalize reads only INITIAL_FIELDS here too (simplified test spec).
        let finalize_spec = StageSpec {
            id: StageId::Finalize,
            reads: &[AnnotationField::RawFactors],
            writes: &[AnnotationField::ScoreBase],
            score_effect: Some(ScoreEffect::Rescore),
        };
        let bad_pipeline = [sanity_spec, finalize_spec];
        let err = validate_pipeline(&bad_pipeline).unwrap_err();
        assert_eq!(
            err,
            ValidationError::RescoreAfterEffect {
                rescore_stage: StageId::Finalize,
                effect_stage: StageId::Sanity,
            }
        );
    }

    /// Negative: two Rescore stages in the pipeline.
    #[test]
    fn negative_two_rescore_stages() {
        let finalize1 = StageSpec {
            id: StageId::Finalize,
            reads: &[AnnotationField::RawFactors],
            writes: &[AnnotationField::ScoreBase],
            score_effect: Some(ScoreEffect::Rescore),
        };
        // Reuse Finalize id to represent a second hypothetical Rescore.
        let finalize2 = StageSpec {
            id: StageId::Finalize,
            reads: &[AnnotationField::ScoreBase],
            writes: &[AnnotationField::ScoreBase],
            score_effect: Some(ScoreEffect::Rescore),
        };
        let bad_pipeline = [finalize1, finalize2];
        let err = validate_pipeline(&bad_pipeline).unwrap_err();
        assert_eq!(err, ValidationError::MultipleRescore);
    }

    /// Negative: PostScoreGate appears before Rescore.
    #[test]
    fn negative_post_score_gate_before_rescore() {
        let killable_spec = StageSpec {
            id: StageId::KillableGate,
            reads: &[AnnotationField::SnapshotFacts],
            writes: &[AnnotationField::ScoreEffects],
            score_effect: Some(ScoreEffect::PostScoreGate),
        };
        let finalize_spec = StageSpec {
            id: StageId::Finalize,
            reads: &[AnnotationField::RawFactors],
            writes: &[AnnotationField::ScoreBase],
            score_effect: Some(ScoreEffect::Rescore),
        };
        // KillableGate (PostScoreGate) before Finalize (Rescore) → error.
        let bad_pipeline = [killable_spec, finalize_spec];
        let err = validate_pipeline(&bad_pipeline).unwrap_err();
        assert_eq!(
            err,
            ValidationError::PostScoreGateBeforeRescore {
                gate: StageId::KillableGate,
                rescore: StageId::Finalize,
            }
        );
    }
}
