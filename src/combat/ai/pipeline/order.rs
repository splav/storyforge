//! Production pipeline declaration — single source of truth for stage order.
//!
//! Used by both production code (`utility/mod.rs::pick_action`) and tests.
//! `run` is the sole dispatch point — zero match-branches on stage identity.
//!
//! # Split pipeline
//!
//! The production pipeline is split into two halves at the natural boundary
//! between Critics (last multiplier stage) and ProtectSelfMask (first
//! mask/gate stage).  This mirrors the `base_scored` snapshot that `pick_action`
//! takes between the two halves: those "post-sanity/critics, pre-mask" scores
//! are carried in `PickResult.base_scored` and used by the decision log.
//!
//! ```text
//! PRE_MASK:  Viability → ItemScoring → ModeSelection → Finalize → Sanity → Critics
//! POST_MASK: ProtectSelfMask → TransitDeathMask → KillableGate → RepairAffinity
//!            → OverlayConsiderations → PlanModifiers → PickBest
//! ```
//!
//! Both slices are `pub const` and use the same `run` runner.

use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};

// ── StageId ───────────────────────────────────────────────────────────────────

/// Identifier for each production pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageId {
    Viability,
    ItemScoring,
    ModeSelection,
    Finalize,
    Sanity,
    Critics,
    ProtectSelfMask,
    TransitDeathMask,
    KillableGate,
    RepairAffinity,
    OverlayConsiderations,
    PlanModifiers,
    PickBest,
}

// ── StageEntry ────────────────────────────────────────────────────────────────

/// One entry in a pipeline table: an identifier plus a thin fn-pointer shim.
///
/// Using fn pointers (instead of `&'static dyn PlanStage`) keeps the table
/// `const`-constructible without running into `&'static dyn Trait` lifetime /
/// Sync / object-safety corners in `const` context.
pub struct StageEntry {
    pub id: StageId,
    pub apply: fn(&mut ScoredPool, &mut StageCtx),
}

// ── Thin shims ────────────────────────────────────────────────────────────────
//
// One function per stage.  Each simply delegates to the stage's `PlanStage::apply`.

fn apply_viability(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::viability::ViabilityStage;
    ViabilityStage.apply(pool, ctx);
}

fn apply_item_scoring(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::item_scoring::ItemScoringStage;
    ItemScoringStage.apply(pool, ctx);
}

fn apply_mode_selection(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::mode_selection::ModeSelectionStage;
    ModeSelectionStage.apply(pool, ctx);
}

fn apply_finalize(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
    FinalizeStage.apply(pool, ctx);
}

fn apply_sanity(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::sanity::SanityStage;
    SanityStage.apply(pool, ctx);
}

fn apply_critics(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::critics::CriticsStage;
    CriticsStage::first_wave().apply(pool, ctx);
}

fn apply_protect_self_mask(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::protect_self::ProtectSelfMaskStage;
    ProtectSelfMaskStage.apply(pool, ctx);
}

fn apply_transit_death_mask(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::transit_death_mask::TransitDeathMaskStage;
    TransitDeathMaskStage.apply(pool, ctx);
}

fn apply_killable_gate(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::killable_gate::KillableGateStage;
    KillableGateStage.apply(pool, ctx);
}

fn apply_repair_affinity(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::repair_affinity::RepairAffinityStage;
    RepairAffinityStage.apply(pool, ctx);
}

fn apply_overlay_considerations(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::overlay_considerations::OverlayConsiderationsStage;
    OverlayConsiderationsStage.apply(pool, ctx);
}

fn apply_plan_modifiers(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::modifiers::PlanModifiersStage;
    PlanModifiersStage.apply(pool, ctx);
}

fn apply_pick_best(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    use crate::combat::ai::pipeline::stages::pick_best::PickBestStage;
    PickBestStage.apply(pool, ctx);
}

// ── Pipeline tables ───────────────────────────────────────────────────────────

/// Pre-mask half: scoring multipliers only. Ends after `Critics` — the last
/// stage that applies multiplicative score effects. `pick_action` snapshots
/// `pool.annotations[*].score` here as `base_scored` before the post-mask half.
pub const PRODUCTION_PIPELINE_PRE_MASK: &[StageEntry] = &[
    StageEntry {
        id: StageId::Viability,
        apply: apply_viability,
    },
    StageEntry {
        id: StageId::ItemScoring,
        apply: apply_item_scoring,
    },
    StageEntry {
        id: StageId::ModeSelection,
        apply: apply_mode_selection,
    },
    StageEntry {
        id: StageId::Finalize,
        apply: apply_finalize,
    },
    StageEntry {
        id: StageId::Sanity,
        apply: apply_sanity,
    },
    StageEntry {
        id: StageId::Critics,
        apply: apply_critics,
    },
];

/// Post-mask half: masks, gates, additive modifiers, and final pick.
///
/// Runs after the `base_scored` snapshot is taken.
pub const PRODUCTION_PIPELINE_POST_MASK: &[StageEntry] = &[
    StageEntry {
        id: StageId::ProtectSelfMask,
        apply: apply_protect_self_mask,
    },
    StageEntry {
        id: StageId::TransitDeathMask,
        apply: apply_transit_death_mask,
    },
    StageEntry {
        id: StageId::KillableGate,
        apply: apply_killable_gate,
    },
    StageEntry {
        id: StageId::RepairAffinity,
        apply: apply_repair_affinity,
    },
    StageEntry {
        id: StageId::OverlayConsiderations,
        apply: apply_overlay_considerations,
    },
    StageEntry {
        id: StageId::PlanModifiers,
        apply: apply_plan_modifiers,
    },
    StageEntry {
        id: StageId::PickBest,
        apply: apply_pick_best,
    },
];

/// Full production pipeline — both halves concatenated, for tests that do not
/// need the mid-pipeline `base_scored` snapshot.
///
/// `pick_action` itself uses the split constants directly.  This constant is
/// the single literal name that test assertions require (DoD).
pub const PRODUCTION_PIPELINE: &[StageEntry] = &[
    StageEntry {
        id: StageId::Viability,
        apply: apply_viability,
    },
    StageEntry {
        id: StageId::ItemScoring,
        apply: apply_item_scoring,
    },
    StageEntry {
        id: StageId::ModeSelection,
        apply: apply_mode_selection,
    },
    StageEntry {
        id: StageId::Finalize,
        apply: apply_finalize,
    },
    StageEntry {
        id: StageId::Sanity,
        apply: apply_sanity,
    },
    StageEntry {
        id: StageId::Critics,
        apply: apply_critics,
    },
    StageEntry {
        id: StageId::ProtectSelfMask,
        apply: apply_protect_self_mask,
    },
    StageEntry {
        id: StageId::TransitDeathMask,
        apply: apply_transit_death_mask,
    },
    StageEntry {
        id: StageId::KillableGate,
        apply: apply_killable_gate,
    },
    StageEntry {
        id: StageId::RepairAffinity,
        apply: apply_repair_affinity,
    },
    StageEntry {
        id: StageId::OverlayConsiderations,
        apply: apply_overlay_considerations,
    },
    StageEntry {
        id: StageId::PlanModifiers,
        apply: apply_plan_modifiers,
    },
    StageEntry {
        id: StageId::PickBest,
        apply: apply_pick_best,
    },
];

// ── Runner ────────────────────────────────────────────────────────────────────

/// Run a pipeline slice on `pool`.  Sole dispatch point — one call per entry,
/// no match on stage identity.
pub fn run(pipeline: &[StageEntry], pool: &mut ScoredPool, ctx: &mut StageCtx) {
    for entry in pipeline {
        (entry.apply)(pool, ctx);
    }
}
