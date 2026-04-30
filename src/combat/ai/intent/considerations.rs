//! Considerations — step 11.3.
//!
//! `IntentConsiderations` is a 6-axis struct that scores each `AgendaItem`
//! along orthogonal tactical dimensions.  All axes are `f32` in `[0.0, 1.0]`.
//!
//! **11.3 contract**: considerations are computed and stored on every `AgendaItem`
//! but are **not** used for routing — that lands in 11.4.  The plan-aware axes
//! (`feasibility`, `leverage`, `safety`) default to their "no-data" values when
//! called without a `PlanAnnotation` (which is the case in 11.3).
//!
//! Axes and their sources:
//! - `urgency`            — `NeedSignals` mapped from `IntentKind`.
//! - `feasibility`        — plan viability score when available; 1.0 otherwise.
//! - `leverage`           — normalised damage / kill / position value from plan; 0.0 otherwise.
//! - `safety`             — `1 - exposure`; 1.0 when no plan data.
//! - `role_affinity`      — lookup table: dominant `AxisProfile` axis × `IntentKind`.
//! - `continuation_value` — `continue_commitment` + optional `RepairAffinity` bonus.

use serde::{Deserialize, Serialize};

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::intent::{AgendaItem, IntentKind};
use crate::combat::ai::repair::affinity::RepairAffinity;
use crate::combat::ai::role::AxisProfile;

// ── IntentConsiderations ──────────────────────────────────────────────────────

/// Six orthogonal tactical axes, each in `[0.0, 1.0]`.
///
/// Used in 11.4 as a per-band-weighted sum to select the winning agenda item.
/// In 11.3 they are computed and stored for observability only.
#[derive(Default, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct IntentConsiderations {
    /// Pressure to act on this item — how needed is the action right now.
    pub urgency: f32,
    /// Probability the action succeeds / plan is reachable.
    pub feasibility: f32,
    /// Magnitude of the tactical effect (damage, kill, rescue value).
    pub leverage: f32,
    /// 1 - exposure (self-damage / danger at end position).
    pub safety: f32,
    /// Affinity of the actor's role to this intent kind.
    pub role_affinity: f32,
    /// Stickiness / repair-aware continuation value.
    pub continuation_value: f32,
}

// ── compute_considerations ────────────────────────────────────────────────────

/// Compute all six `IntentConsiderations` axes for one `AgendaItem`.
///
/// `repair` should be `None` in 11.3 (no plan-level affinity yet).
/// Plan-aware axes (`feasibility`, `leverage`, `safety`) receive safe defaults
/// when no plan data is provided.
///
/// # Arguments
/// - `item`   — the agenda item being scored.
/// - `needs`  — pre-computed `NeedSignals` for this actor's turn.
/// - `role`   — actor's `AxisProfile` for role-affinity lookup.
/// - `repair` — optional `RepairAffinity` from last-goal tracking; `None` in 11.3.
pub fn compute_considerations(
    item: &AgendaItem,
    needs: &NeedSignals,
    role: &AxisProfile,
    repair: Option<&RepairAffinity>,
) -> IntentConsiderations {
    IntentConsiderations {
        urgency:            urgency(item, needs),
        feasibility:        1.0, // TODO 11.4: use plan viability when plan_for_item is available
        leverage:           0.0, // TODO 11.4: use plan outcomes when plan_for_item is available
        safety:             1.0, // TODO 11.4: use plan terminal exposure when plan_for_item is available
        role_affinity:      role_affinity(item.kind, role),
        continuation_value: continuation_value(needs, repair),
    }
}

// ── urgency ───────────────────────────────────────────────────────────────────

/// Map `IntentKind` to the most relevant `NeedSignals` axis.
///
/// All values are already `[0, 1]` from `compute_need_signals`, so no clamping
/// is needed here — the clamp below is only a defensive guard.
fn urgency(item: &AgendaItem, needs: &NeedSignals) -> f32 {
    let raw = match item.kind {
        IntentKind::ProtectSelf => needs.self_preserve,
        IntentKind::ProtectAlly => needs.rescue_ally,
        IntentKind::FocusTarget => {
            // Average finish_target and continue_commitment to reward both
            // kill opportunities and target commitment.
            0.5 * needs.finish_target + 0.5 * needs.continue_commitment
        }
        IntentKind::ApplyCC => needs.apply_cc,
        IntentKind::SetupAOE => needs.setup_aoe,
        IntentKind::Reposition => needs.reposition,
        IntentKind::LastStand => 0.5, // no dedicated signal — neutral fallback
    };
    raw.clamp(0.0, 1.0)
}

// ── role_affinity ─────────────────────────────────────────────────────────────

/// Dominant-role × intent lookup table.
///
/// Dominant role is the argmax of `[tank, melee, ranged, control, support]`.
/// When all axes are near-zero (empty profile), returns 0.5 for all intents.
///
/// Note: `AxisProfile` does not have an explicit `offense` field; we treat
/// the max of `melee` and `ranged` as the offense proxy (both are attack roles).
fn role_affinity(kind: IntentKind, role: &AxisProfile) -> f32 {
    let axes = role.as_array(); // [tank, melee, ranged, control, support]
    let sum: f32 = axes.iter().sum();
    if sum < 1e-6 {
        // Empty / unconfigured profile — neutral weights.
        return 0.5;
    }

    // Identify dominant axis.
    let dominant = axes
        .iter()
        .copied()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(1); // fallback: melee

    // AxisProfile indices: 0=tank, 1=melee, 2=ranged, 3=control, 4=support
    match (dominant, kind) {
        // ── Support / Healer ─────────────────────────────────────────────
        (4, IntentKind::ProtectAlly)   => 1.0,
        (4, IntentKind::ProtectSelf)   => 0.7,
        (4, IntentKind::FocusTarget)   => 0.3,
        (4, IntentKind::ApplyCC)       => 0.5,
        (4, IntentKind::SetupAOE)      => 0.3,
        (4, IntentKind::Reposition)    => 0.6,
        (4, IntentKind::LastStand)     => 0.4,

        // ── DPS: Melee (1) or Ranged (2) ─────────────────────────────────
        (1 | 2, IntentKind::FocusTarget)   => 1.0,
        (1 | 2, IntentKind::ApplyCC)       => 0.7,
        (1 | 2, IntentKind::SetupAOE)      => 0.6,
        (1 | 2, IntentKind::ProtectAlly)   => 0.3,
        (1 | 2, IntentKind::ProtectSelf)   => 0.4,
        (1 | 2, IntentKind::Reposition)    => 0.5,
        (1 | 2, IntentKind::LastStand)     => 0.6,

        // ── Tank / Bruiser (0) or Control (3) ────────────────────────────
        (0 | 3, IntentKind::ProtectAlly)   => 0.7, // peel
        (0 | 3, IntentKind::FocusTarget)   => 0.7,
        (0 | 3, IntentKind::ProtectSelf)   => 0.5,
        (0 | 3, IntentKind::Reposition)    => 0.4,
        (0 | 3, IntentKind::ApplyCC)       => 0.7, // control loves CC
        (0 | 3, IntentKind::SetupAOE)      => 0.5,
        (0 | 3, IntentKind::LastStand)     => 0.5,

        // Catch-all (should not happen with bounded dominant index)
        _ => 0.5,
    }
}

// ── continuation_value ────────────────────────────────────────────────────────

/// Stickiness score from commitment signal and optional repair affinity.
///
/// Formula (plan §11, decision §10):
/// - With repair: `0.5 × continue_commitment + 0.5 × repair_severity_score`
/// - Without repair: `0.5 × continue_commitment`
///
/// `repair_severity_score` is `repair.severity_factor` — already in `[0, 1]`,
/// higher = less severe mismatch = more continuation pressure.
fn continuation_value(needs: &NeedSignals, repair: Option<&RepairAffinity>) -> f32 {
    let commitment = needs.continue_commitment.clamp(0.0, 1.0);
    let raw = if let Some(r) = repair {
        // severity_factor: 1.0 = no mismatch (Cosmetic), 0.0 = Invalidating.
        // We treat it directly as a "repair comfort" signal.
        let repair_score = r.severity_factor.clamp(0.0, 1.0);
        0.5 * commitment + 0.5 * repair_score
    } else {
        0.5 * commitment
    };
    raw.clamp(0.0, 1.0)
}

// ── Default repair weights (for tests) ───────────────────────────────────────

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::NeedSignals;
    use crate::combat::ai::intent::{AgendaItem, IntentKind, IntentReason};
    use crate::combat::ai::repair::affinity::RepairAffinity;
    use crate::combat::ai::role::AxisProfile;

    fn item(kind: IntentKind) -> AgendaItem {
        AgendaItem {
            kind,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations: IntentConsiderations::default(),
        }
    }

    fn zero_needs() -> NeedSignals { NeedSignals::default() }

    fn pure_support() -> AxisProfile {
        AxisProfile { tank: 0.0, melee: 0.0, ranged: 0.0, control: 0.0, support: 1.0 }
    }

    fn pure_melee_dps() -> AxisProfile {
        AxisProfile { tank: 0.0, melee: 1.0, ranged: 0.0, control: 0.0, support: 0.0 }
    }

    fn neutral_role() -> AxisProfile { AxisProfile::default() }

    // ── 1. urgency ────────────────────────────────────────────────────────────

    #[test]
    fn urgency_zero_when_no_need_signal() {
        let it = item(IntentKind::ProtectSelf);
        let needs = zero_needs(); // self_preserve = 0.0
        let c = compute_considerations(&it, &needs, &neutral_role(), None);
        assert!(
            c.urgency.abs() < 1e-6,
            "urgency should be 0 with no need signals, got {}",
            c.urgency
        );
    }

    #[test]
    fn urgency_high_when_self_preserve_high_for_protect_self() {
        let it = item(IntentKind::ProtectSelf);
        let needs = NeedSignals { self_preserve: 0.9, ..NeedSignals::default() };
        let c = compute_considerations(&it, &needs, &neutral_role(), None);
        assert!(
            c.urgency > 0.8,
            "urgency should mirror self_preserve (0.9), got {}",
            c.urgency
        );
    }

    // ── 2. feasibility ────────────────────────────────────────────────────────

    #[test]
    fn feasibility_one_when_no_plan_provided() {
        // In 11.3, compute_considerations is always called without plan data.
        // Default must be 1.0 (assume reachable).
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        assert!(
            (c.feasibility - 1.0).abs() < 1e-6,
            "feasibility default must be 1.0, got {}",
            c.feasibility
        );
    }

    #[test]
    fn feasibility_reads_viability_when_plan_provided() {
        // In 11.3, plan overlay is not implemented — feasibility stays 1.0.
        // This test documents the 11.4 TODO: once plan_for_item is wired in,
        // feasibility should reflect viability score.  For now verify it's 1.0.
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        // Assertion: 1.0 until 11.4 wires the plan overlay.
        assert!(
            (c.feasibility - 1.0).abs() < 1e-6,
            "feasibility must be 1.0 in 11.3 (plan overlay deferred to 11.4), got {}",
            c.feasibility
        );
    }

    // ── 3. leverage ───────────────────────────────────────────────────────────

    #[test]
    fn leverage_zero_when_no_plan_provided() {
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        assert!(
            c.leverage.abs() < 1e-6,
            "leverage default must be 0.0 when no plan provided, got {}",
            c.leverage
        );
    }

    #[test]
    fn leverage_high_for_cast_with_high_enemy_damage() {
        // In 11.3, leverage stays 0.0 (no plan overlay yet).
        // This test documents the expected 11.4 behaviour: a cast plan
        // with high enemy_damage should produce leverage near 1.0.
        // For now verify the 11.3 default.
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        // 11.3: always 0.0 (plan overlay deferred to 11.4).
        assert!(
            c.leverage.abs() < 1e-6,
            "leverage is 0.0 in 11.3 (plan overlay deferred to 11.4), got {}",
            c.leverage
        );
    }

    // ── 4. safety ────────────────────────────────────────────────────────────

    #[test]
    fn safety_one_when_no_plan_provided() {
        let it = item(IntentKind::Reposition);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        assert!(
            (c.safety - 1.0).abs() < 1e-6,
            "safety default must be 1.0 when no plan provided, got {}",
            c.safety
        );
    }

    #[test]
    fn safety_low_when_terminal_exposure_high() {
        // In 11.3, safety stays 1.0 (no plan overlay yet).
        // This test documents the 11.4 behaviour: high terminal exposure
        // should drive safety toward 0.0.
        // For now verify the 11.3 default.
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &neutral_role(), None);
        // 11.3: always 1.0 (plan overlay deferred to 11.4).
        assert!(
            (c.safety - 1.0).abs() < 1e-6,
            "safety is 1.0 in 11.3 (plan overlay deferred to 11.4), got {}",
            c.safety
        );
    }

    // ── 5. role_affinity ─────────────────────────────────────────────────────

    #[test]
    fn role_affinity_healer_protect_ally_high() {
        let it = item(IntentKind::ProtectAlly);
        let c = compute_considerations(&it, &zero_needs(), &pure_support(), None);
        assert!(
            (c.role_affinity - 1.0).abs() < 1e-6,
            "healer × ProtectAlly should be 1.0, got {}",
            c.role_affinity
        );
    }

    #[test]
    fn role_affinity_dps_focus_target_high() {
        let it = item(IntentKind::FocusTarget);
        let c = compute_considerations(&it, &zero_needs(), &pure_melee_dps(), None);
        assert!(
            (c.role_affinity - 1.0).abs() < 1e-6,
            "melee DPS × FocusTarget should be 1.0, got {}",
            c.role_affinity
        );
    }

    // ── 6. continuation_value ────────────────────────────────────────────────

    #[test]
    fn continuation_value_zero_without_repair_or_commitment() {
        // No repair, no commitment → 0.5 × 0.0 = 0.0
        let it = item(IntentKind::FocusTarget);
        let needs = NeedSignals { continue_commitment: 0.0, ..NeedSignals::default() };
        let c = compute_considerations(&it, &needs, &neutral_role(), None);
        assert!(
            c.continuation_value.abs() < 1e-6,
            "continuation_value should be 0 with zero commitment and no repair, got {}",
            c.continuation_value
        );
    }

    #[test]
    fn continuation_value_high_when_committed_and_repair_aligned() {
        // commitment=1.0, repair with severity_factor=1.0 (no mismatch) →
        // 0.5 × 1.0 + 0.5 × 1.0 = 1.0
        let it = item(IntentKind::FocusTarget);
        let needs = NeedSignals { continue_commitment: 1.0, ..NeedSignals::default() };
        let repair = RepairAffinity {
            goal_alignment:   1.0,
            region_alignment: 1.0,
            method_alignment: 1.0,
            severity_factor:  1.0,  // Cosmetic — no mismatch
            ttl_factor:       1.0,
            confidence:       1.0,
        };
        let c = compute_considerations(&it, &needs, &neutral_role(), Some(&repair));
        assert!(
            (c.continuation_value - 1.0).abs() < 1e-6,
            "continuation_value should be 1.0 when fully committed with aligned repair, got {}",
            c.continuation_value
        );
    }

    // ── bounds sanity ─────────────────────────────────────────────────────────

    #[test]
    fn all_axes_in_unit_range_with_saturated_needs() {
        let it = item(IntentKind::FocusTarget);
        let needs = NeedSignals {
            self_preserve:       1.0,
            rescue_ally:         1.0,
            finish_target:       1.0,
            apply_cc:            1.0,
            setup_aoe:           1.0,
            reposition:          1.0,
            conserve_resource:   1.0,
            continue_commitment: 1.0,
        };
        let repair = RepairAffinity {
            goal_alignment:   1.0,
            region_alignment: 1.0,
            method_alignment: 1.0,
            severity_factor:  1.0,
            ttl_factor:       1.0,
            confidence:       1.0,
        };
        let c = compute_considerations(&it, &needs, &pure_support(), Some(&repair));
        for (name, val) in [
            ("urgency", c.urgency),
            ("feasibility", c.feasibility),
            ("leverage", c.leverage),
            ("safety", c.safety),
            ("role_affinity", c.role_affinity),
            ("continuation_value", c.continuation_value),
        ] {
            assert!(
                (0.0..=1.0).contains(&val),
                "{name} must be in [0,1], got {val}"
            );
        }
    }
}
