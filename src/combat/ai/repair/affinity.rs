/// Step 6.2 — repair affinity computation (read-only).
///
/// `RepairAffinity` is a compositional breakdown of how well a fresh plan
/// aligns with a stored `StoredGoalContext`. Each axis lives in `[0, 1]`
/// and is aggregated into a single bonus via `RepairWeights` in 6.3.
///
/// All-zero input (no stored goal, or `Default::default()`) → `aggregate` = 0,
/// so the scorer is unaffected when no goal is stored.
use serde::{Deserialize, Serialize};

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::plan::types::PlanStep;
use crate::combat::ai::repair::goal::{GoalKind, StoredGoalContext};
use crate::combat::ai::repair::ContinuationSeverity;
use crate::game::hex::Hex;

// ── RepairAffinity ────────────────────────────────────────────────────────────

/// Compositional affinity components — each axis is `0..1`.
///
/// All-zeros means no stored goal exists or the goal was abandoned.
/// Aggregated into a single repair bonus via [`RepairAffinity::aggregate`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RepairAffinity {
    /// How well the fresh intent matches the stored goal kind and target.
    /// `1.0` = exact match; `0.85` = same target, weaker match;
    /// `0.0` = goal abandoned.
    pub goal_alignment: f32,
    /// Fresh plan's final position is within the stored `region_radius` of
    /// `region_anchor`. Decays linearly with distance; `0.0` outside radius.
    pub region_alignment: f32,
    /// Fresh plan's `step[1]` matches the stored `planned_ability`.
    /// `1.0` if match; `0.0` otherwise (no ability stored, no Cast at step 1,
    /// or different ability).
    pub method_alignment: f32,
    /// Multiplicative gate for mismatch severity.
    /// `Cosmetic` → `1.0`, `Relevant` → `0.7`, `Invalidating` → `0.0`.
    /// `None` (no stored plan) → `1.0` (no penalty).
    pub severity_factor: f32,
    /// TTL decay multiplier. `1.0` when freshly created; `0.0` when expired.
    pub ttl_factor: f32,
    /// Scorer confidence at store time (`chosen.score / pool_max`).
    pub confidence: f32,
}

impl RepairAffinity {
    /// Aggregate into a single bonus in `[0, +inf)`.
    ///
    /// Multiplicative gates (`severity_factor`, `ttl_factor`, `confidence`)
    /// can reduce the bonus to `0.0`. All-zero input → `0.0`.
    pub fn aggregate(&self, weights: &RepairWeights) -> f32 {
        let combined = self.goal_alignment * weights.goal_w
            + self.region_alignment * weights.region_w
            + self.method_alignment * weights.method_w;
        combined * self.severity_factor * self.ttl_factor * self.confidence
    }
}

// ── RepairWeights ─────────────────────────────────────────────────────────────

/// Role-mixed weights for the three additive axes of `RepairAffinity`.
#[derive(Debug, Clone, Copy)]
pub struct RepairWeights {
    pub goal_w: f32,
    pub region_w: f32,
    pub method_w: f32,
}

// ── Producer ─────────────────────────────────────────────────────────────────

/// Compute the repair affinity of a fresh plan against the stored goal.
///
/// # Parameters
/// - `fresh_intent` — tactical intent of the fresh plan (one intent per pool).
/// - `fresh_steps` — step sequence of the fresh plan (for method alignment).
/// - `fresh_final_pos` — final hex position after all steps (for region alignment).
/// - `stored` — the stored goal context from `AiMemory.last_goal`.
/// - `severity` — optional mismatch severity from `PlanSnapshot::check_continuation`.
///   `None` means no stored plan / no mismatch → multiplicative factor = `1.0`.
/// - `current_round` — current combat round (for TTL decay).
pub fn compute_repair_affinity(
    fresh_intent: TacticalIntent,
    fresh_steps: &[PlanStep],
    fresh_final_pos: Hex,
    stored: &StoredGoalContext,
    severity: Option<ContinuationSeverity>,
    current_round: u32,
) -> RepairAffinity {
    let goal_alignment = goal_alignment(&stored.kind, fresh_intent);
    let region_alignment = region_alignment(fresh_final_pos, stored);
    let method_alignment = method_alignment(fresh_steps, stored);
    let severity_factor = severity_factor(severity);
    let ttl_factor = ttl_factor(stored, current_round);
    let confidence = stored.confidence;

    RepairAffinity {
        goal_alignment,
        region_alignment,
        method_alignment,
        severity_factor,
        ttl_factor,
        confidence,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn goal_alignment(stored_kind: &GoalKind, fresh_intent: TacticalIntent) -> f32 {
    match (stored_kind, fresh_intent) {
        // Exact goal + same target
        (GoalKind::Finish { target: a }, TacticalIntent::FocusTarget { target: b }) if *a == b => {
            1.0
        }
        (GoalKind::Pressure { target: a }, TacticalIntent::FocusTarget { target: b })
            if *a == b =>
        {
            0.85
        }
        (GoalKind::DisableEnemy { target: a }, TacticalIntent::ApplyCC { target: b })
            if *a == b =>
        {
            1.0
        }
        (GoalKind::HealAlly { ally: a }, TacticalIntent::ProtectAlly { ally: b }) if *a == b => {
            1.0
        }
        // Retreat matches ProtectSelf — LastStand is now an EvaluationMode, not
        // a TacticalIntent; Retreat affinity for adapted plans is covered via ProtectSelf.
        (GoalKind::Retreat { .. }, TacticalIntent::ProtectSelf) => 0.9,
        // Positional goals match their own intent
        (GoalKind::SetupAOE { .. }, TacticalIntent::SetupAOE) => 0.95,
        (GoalKind::Reposition { .. }, TacticalIntent::Reposition) => 0.85,
        // Cross-goal partial credit — same target but different method
        (GoalKind::Finish { target: a }, TacticalIntent::ApplyCC { target: b }) if *a == b => {
            0.4
        }
        (GoalKind::Pressure { target: a }, TacticalIntent::ApplyCC { target: b }) if *a == b => {
            0.4
        }
        // Step 6.8.B: target switch within the same intent (FocusTarget on a
        // different enemy) — partial credit. Recognises that the actor is still
        // "in attack mode" and provides a small commitment nudge against
        // marginal target-hopping, without suppressing clearly-better switches
        // (sd > 0.5 still wins easily).
        (GoalKind::Finish { target: a }, TacticalIntent::FocusTarget { target: b }) if *a != b => {
            0.3
        }
        (GoalKind::Pressure { target: a }, TacticalIntent::FocusTarget { target: b })
            if *a != b =>
        {
            0.3
        }
        // Any other combination — goal abandoned
        _ => 0.0,
    }
}

fn region_alignment(fresh_final_pos: Hex, stored: &StoredGoalContext) -> f32 {
    let dist = fresh_final_pos.unsigned_distance_to(stored.region_anchor);
    if dist <= stored.region_radius {
        // `+1` denominator prevents div-by-zero when region_radius == 0.
        // When dist == 0 and radius == 0: 1 - 0/1 = 1.0 (exact tile match).
        // When dist == 1 and radius == 0: 1 > 0 so outer branch → 0.0.
        1.0 - dist as f32 / (stored.region_radius + 1) as f32
    } else {
        0.0
    }
}

fn method_alignment(fresh_steps: &[PlanStep], stored: &StoredGoalContext) -> f32 {
    match (&stored.planned_ability, fresh_steps.get(1)) {
        (Some(stored_ab), Some(PlanStep::Cast { ability, .. })) if stored_ab == ability => 1.0,
        _ => 0.0,
    }
}

fn severity_factor(severity: Option<ContinuationSeverity>) -> f32 {
    match severity {
        None => 1.0,
        Some(ContinuationSeverity::Cosmetic) => 1.0,
        Some(ContinuationSeverity::Relevant) => 0.7,
        Some(ContinuationSeverity::Invalidating) => 0.0,
    }
}

fn ttl_factor(stored: &StoredGoalContext, current_round: u32) -> f32 {
    if stored.ttl == 0 {
        // TTL of 0 means "already expired at creation" — no bonus.
        return 0.0;
    }
    let age = current_round.saturating_sub(stored.created_round);
    if age >= stored.ttl as u32 {
        0.0
    } else {
        1.0 - age as f32 / stored.ttl as f32
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::repair::goal::{GoalKind, StoredGoalContext};
    use crate::core::AbilityId;
    use crate::game::hex::Hex;
    use bevy::prelude::Entity;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn stored(kind: GoalKind, anchor: Hex, radius: u32, ability: Option<&str>) -> StoredGoalContext {
        StoredGoalContext {
            kind,
            region_anchor: anchor,
            region_radius: radius,
            planned_ability: ability.map(AbilityId::from),
            ttl: 2,
            confidence: 1.0,
            created_round: 1,
            // Severity-check fields zeroed — affinity tests don't exercise check_continuation.
            expected_actor_pos: anchor,
            actor_hp_at_store: 0,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            actor_statuses_at_store: vec![],
            target_hp_at_store: 0,
            target_pos_at_store: Hex::ZERO,
        }
    }

    fn weights() -> RepairWeights {
        // Equal weights for test clarity
        RepairWeights { goal_w: 1.0, region_w: 1.0, method_w: 1.0 }
    }

    // ── goal_alignment ────────────────────────────────────────────────────────

    #[test]
    fn goal_alignment_perfect_for_same_target_finish() {
        let target = ent(1);
        let s = stored(GoalKind::Finish { target }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target },
            &[],
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.goal_alignment - 1.0).abs() < 1e-6);
    }

    /// Step 6.8.B: target switch within FocusTarget gets partial credit (0.3),
    /// not zero — the actor is still in attack mode, just retargeting.
    #[test]
    fn goal_alignment_partial_for_target_switch_within_focus_target() {
        let a = ent(1);
        let b = ent(2);
        let s = stored(GoalKind::Finish { target: a }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target: b },
            &[],
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.goal_alignment - 0.3).abs() < 1e-6);
        // Also the symmetric case from Pressure stored.
        let s2 = stored(GoalKind::Pressure { target: a }, Hex::ZERO, 2, None);
        let affinity2 = compute_repair_affinity(
            TacticalIntent::FocusTarget { target: b },
            &[],
            Hex::ZERO,
            &s2,
            None,
            1,
        );
        assert!((affinity2.goal_alignment - 0.3).abs() < 1e-6);
    }

    /// Genuine cross-intent abandon (Retreat → FocusTarget) still scores 0.
    #[test]
    fn goal_alignment_zero_for_cross_intent_abandon() {
        let target = ent(1);
        let s = stored(GoalKind::Retreat { region_anchor: Hex::ZERO }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target },
            &[],
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.goal_alignment - 0.0).abs() < 1e-6);
    }

    #[test]
    fn goal_alignment_partial_for_finish_to_apply_cc_same_target() {
        let target = ent(1);
        let s = stored(GoalKind::Finish { target }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::ApplyCC { target },
            &[],
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.goal_alignment - 0.4).abs() < 1e-6);
    }

    // ── region_alignment ─────────────────────────────────────────────────────

    #[test]
    fn region_alignment_full_at_anchor() {
        // dist == 0, radius == 2 → 1 - 0/3 = 1.0
        let target = ent(1);
        let anchor = Hex::new(3, 0);
        let s = stored(GoalKind::Reposition { region_center: anchor }, anchor, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::Reposition,
            &[],
            anchor, // fresh_final_pos == anchor → dist 0
            &s,
            None,
            1,
        );
        assert!((affinity.region_alignment - 1.0).abs() < 1e-6, "expected 1.0, got {}", affinity.region_alignment);
        let _ = target;
    }

    #[test]
    fn region_alignment_decays_with_distance() {
        // dist == 1, radius == 2 → 1 - 1/3 ≈ 0.666...
        let anchor = Hex::new(0, 0);
        let fresh_pos = Hex::new(1, 0); // distance 1 from anchor
        let s = stored(GoalKind::Reposition { region_center: anchor }, anchor, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::Reposition,
            &[],
            fresh_pos,
            &s,
            None,
            1,
        );
        let expected = 1.0 - 1.0_f32 / 3.0;
        assert!((affinity.region_alignment - expected).abs() < 1e-5, "expected {expected}, got {}", affinity.region_alignment);
    }

    #[test]
    fn region_alignment_zero_outside_radius() {
        // dist == 3, radius == 2 → 0.0
        let anchor = Hex::new(0, 0);
        let fresh_pos = Hex::new(3, 0); // distance 3 from anchor
        let s = stored(GoalKind::Reposition { region_center: anchor }, anchor, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::Reposition,
            &[],
            fresh_pos,
            &s,
            None,
            1,
        );
        assert!((affinity.region_alignment - 0.0).abs() < 1e-6, "expected 0.0, got {}", affinity.region_alignment);
    }

    // ── method_alignment ─────────────────────────────────────────────────────

    #[test]
    fn method_alignment_set_when_planned_ability_matches() {
        let target = ent(1);
        let ability_id = AbilityId::from("fireball");
        let s = stored(GoalKind::SetupAOE { region_center: Hex::ZERO, planned_ability: ability_id.clone() }, Hex::ZERO, 2, Some("fireball"));
        let cast_step = PlanStep::Cast {
            ability: ability_id,
            target,
            target_pos: Hex::ZERO,
        };
        let steps = vec![
            PlanStep::Move { path: vec![Hex::new(1, 0)] },
            cast_step,
        ];
        let affinity = compute_repair_affinity(
            TacticalIntent::SetupAOE,
            &steps,
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.method_alignment - 1.0).abs() < 1e-6);
    }

    #[test]
    fn method_alignment_zero_when_no_planned_ability() {
        let s = stored(GoalKind::Reposition { region_center: Hex::ZERO }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::Reposition,
            &[],
            Hex::ZERO,
            &s,
            None,
            1,
        );
        assert!((affinity.method_alignment - 0.0).abs() < 1e-6);
    }

    // ── severity_factor ───────────────────────────────────────────────────────

    #[test]
    fn severity_factor_invalidating_zeros_aggregate() {
        let target = ent(1);
        // Full match everywhere, but severity = Invalidating → aggregate = 0
        let anchor = Hex::ZERO;
        let s = stored(GoalKind::Finish { target }, anchor, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target },
            &[],
            anchor,
            &s,
            Some(ContinuationSeverity::Invalidating),
            1,
        );
        let bonus = affinity.aggregate(&weights());
        assert!((bonus - 0.0).abs() < 1e-6, "expected aggregate=0.0, got {bonus}");
    }

    // ── ttl_factor ────────────────────────────────────────────────────────────

    #[test]
    fn ttl_factor_zero_when_age_exceeds_ttl() {
        // created_round=1, ttl=2, current_round=3 → age=2 >= ttl=2 → 0.0
        let target = ent(1);
        let s = stored(GoalKind::Finish { target }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target },
            &[],
            Hex::ZERO,
            &s,
            None,
            3, // current_round
        );
        assert!((affinity.ttl_factor - 0.0).abs() < 1e-6, "expected ttl_factor=0.0, got {}", affinity.ttl_factor);
    }

    #[test]
    fn ttl_factor_decays_linearly() {
        // created_round=1, ttl=2, current_round=2 → age=1, factor = 1 - 1/2 = 0.5
        let target = ent(1);
        let s = stored(GoalKind::Finish { target }, Hex::ZERO, 2, None);
        let affinity = compute_repair_affinity(
            TacticalIntent::FocusTarget { target },
            &[],
            Hex::ZERO,
            &s,
            None,
            2, // current_round
        );
        assert!((affinity.ttl_factor - 0.5).abs() < 1e-6, "expected ttl_factor=0.5, got {}", affinity.ttl_factor);
    }

    // ── aggregate ────────────────────────────────────────────────────────────

    #[test]
    fn aggregate_no_stored_goal_yields_zero() {
        // Default-initialized affinity (all zeros) → aggregate = 0 for any weights
        let affinity = RepairAffinity::default();
        let bonus = affinity.aggregate(&weights());
        assert!((bonus - 0.0).abs() < 1e-6, "expected 0.0, got {bonus}");
    }
}
