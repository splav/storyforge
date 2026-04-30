//! Tiered killable gate — hard mask under `TacticalIntent::FocusTarget`.
//!
//! Pipeline position: after `apply_adaptation`, before the picker.
//! The gate enforces the contract "if a kill is reachable against the
//! intent target, the winning plan must pursue that kill".
//!
//! ## Tier table
//!
//! | Strength | Keep predicate | Closes metric |
//! |----------|---------------|---------------|
//! | `None`   | `true` (no-op) | — |
//! | `Pressure` | `offensive_vs_target(plan, target)` | `killable_non_offensive_rate < 2%` |
//! | `CanFinish` | `offensive_vs_target(plan, target) ∧ kill_now ≥ 1` | `kill_conversion_rate > 85%` |
//!
//! ## Key invariants
//!
//! 1. **Live-pool filter** — strength detection and the keep-predicate both
//!    operate only on plans where `mode == Default && scores[i].is_finite()`.
//!    Plans already masked to `-∞` (sanity/adaptation/any future layer) do
//!    not raise strength and are not double-pruned. See `docs/ai_rework.md §3.2`.
//!
//! 2. **Intent-coherent detection** — both `Pressure` and `CanFinish` require
//!    `plan_is_offensive_vs(plan, target)` in the strength predicate AND in the
//!    keep predicate. Without this a collateral AoE kill on some *other* enemy
//!    (kn=1 on `step.target != intent_target`) would spuriously raise strength
//!    to `CanFinish` and prune legit offensive-vs-target plans. See
//!    `docs/ai_rework.md §3.1`, §3.2a.

use crate::combat::ai::factors::{PlanFactorValues, StepFactor};
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::adaptation::EvaluationMode;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::world::snapshot::BattleSnapshot;
use bevy::prelude::Entity;

// ── Types ────────────────────────────────────────────────────────────────────

/// Detected kill-line strength against the `FocusTarget` intent target.
///
/// Used both to select the tier and to report telemetry via `GateStats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KillLineStrength {
    /// No plan in the live pool is both offensive vs the intent target
    /// and has meaningful damage or a kill line. Gate is a no-op.
    #[default]
    None,
    /// At least one live plan is offensive vs target with `damage ≥ hp·α`.
    /// Non-offensive plans are pruned.
    Pressure,
    /// At least one live plan is offensive vs target with `kill_now ≥ 1`.
    /// Non-offensive and non-killing plans are pruned.
    CanFinish,
}

/// Telemetry produced by a single gate run.
#[derive(Clone, Copy, Debug, Default)]
pub struct GateStats {
    /// `true` if the gate ran and found strength ≥ `Pressure`.
    pub applied: bool,
    /// Detected kill-line tier.
    pub strength: KillLineStrength,
    /// How many plans were pruned (set to `-∞`) by this gate call.
    pub pruned_count: usize,
}

// KEEP IN SYNC with src/bin/replay_ai_log.rs::KILLABLE_ALPHA
/// Damage threshold as a fraction of target HP for `Pressure` tier.
/// A plan with `damage >= target_hp * KILLABLE_ALPHA` that is also
/// `offensive_vs_target` is considered "real kill pressure".
pub const KILLABLE_ALPHA: f32 = 0.3;

// ── Helper ───────────────────────────────────────────────────────────────────

/// Returns `true` if `plan` has at least one `Cast` step whose `target`
/// field exactly matches `target`.
///
/// AoE casts aimed at another tile that happen to cover the target are NOT
/// counted — this mirrors the diagnostic metric in `replay_ai_log.rs` so
/// gate and measurement see exactly the same truth. Only explicit targeting
/// of the intent target qualifies.
pub fn plan_is_offensive_vs(plan: &TurnPlan, target: Entity) -> bool {
    plan.steps
        .iter()
        .any(|s| matches!(s, PlanStep::Cast { target: t, .. } if *t == target))
}

// ── Main function ────────────────────────────────────────────────────────────

/// Apply the tiered killable gate to the plan score column.
///
/// Must only be called when `intent` is `TacticalIntent::FocusTarget { .. }`.
/// The caller in `ranking.rs` guards this with `matches!(intent, FocusTarget { .. })`.
///
/// Returns a `GateStats` summary suitable for logging.
pub fn apply_killable_gate(
    plans: &[TurnPlan],
    raw: &[PlanFactorValues],
    scores: &mut [f32],
    modes: &[EvaluationMode],
    intent: &TacticalIntent,
    snap: &BattleSnapshot,
) -> GateStats {
    let TacticalIntent::FocusTarget { target } = *intent else {
        return GateStats::default();
    };
    let Some(t) = snap.unit(target) else {
        return GateStats::default();
    };
    let hp_f = t.hp.max(0) as f32;

    // Live pool: survivors of adaptation + any prior hard mask.
    // Sanity soft penalties leave scores finite → plan stays in consideration.
    // Plans at -∞ or in non-Default mode are invisible to this gate.
    let live: Vec<usize> = (0..plans.len())
        .filter(|&i| matches!(modes[i], EvaluationMode::Default))
        .filter(|&i| scores[i].is_finite())
        .collect();
    if live.is_empty() {
        return GateStats::default();
    }

    // Strength detection: intent-coherent on the live pool only.
    // A plan must be offensive_vs_target AND provide the kill signal.
    // Without the offensive_vs_target guard, a collateral AoE kill on
    // some other enemy (kn=1, step.target != intent_target) would spuriously
    // raise strength to CanFinish.
    let can_finish = live.iter().any(|&i| {
        plan_is_offensive_vs(&plans[i], target)
            && raw[i].get(StepFactor::KillNow) >= 1.0
    });
    let has_pressure = live.iter().any(|&i| {
        plan_is_offensive_vs(&plans[i], target)
            && raw[i].get(StepFactor::Damage) >= hp_f * KILLABLE_ALPHA
    });

    let strength = match (can_finish, has_pressure) {
        (true, _) => KillLineStrength::CanFinish,
        (false, true) => KillLineStrength::Pressure,
        _ => return GateStats::default(),
    };

    // Apply keep-set. Only prune indices in `live`; plans already at -∞
    // or in non-Default mode are left untouched (they're already masked).
    let mut pruned = 0usize;
    for &i in &live {
        let keep = match strength {
            KillLineStrength::None => true,
            KillLineStrength::Pressure => plan_is_offensive_vs(&plans[i], target),
            KillLineStrength::CanFinish => {
                plan_is_offensive_vs(&plans[i], target)
                    && raw[i].get(StepFactor::KillNow) >= 1.0
            }
        };
        if !keep {
            scores[i] = f32::NEG_INFINITY;
            pruned += 1;
        }
    }

    GateStats { applied: true, strength, pruned_count: pruned }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::planning::adaptation::EvaluationMode;
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{ent, UnitBuilder};
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    // ── Fixtures ─────────────────────────────────────────────────────────

    fn empty_plan() -> TurnPlan {
        TurnPlan { steps: vec![], ..TurnPlan::default() }
    }

    fn cast_plan(target: Entity) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target,
                target_pos: hex_from_offset(0, 0),
            }],
            ..TurnPlan::default()
        }
    }

    fn cast_plan_at(target: Entity, target_pos: (i32, i32)) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target,
                target_pos: hex_from_offset(target_pos.0, target_pos.1),
            }],
            ..TurnPlan::default()
        }
    }

    fn move_plan() -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }],
            ..TurnPlan::default()
        }
    }

    /// Build a snapshot with `intent_target` at the given HP.
    fn snap_with_target(target_id: u32, hp: i32) -> BattleSnapshot {
        let target = UnitBuilder::new(target_id, Team::Player, hex_from_offset(2, 0))
            .hp(hp)
            .max_hp(20)
            .build();
        BattleSnapshot::new(vec![target], 1)
    }

    fn default_modes(n: usize) -> Vec<EvaluationMode> {
        vec![EvaluationMode::Default; n]
    }

    fn factors_with(damage: f32, kill_now: f32) -> PlanFactorValues {
        let mut f = PlanFactorValues::default();
        f.set(StepFactor::Damage, damage);
        f.set(StepFactor::KillNow, kill_now);
        f
    }

    // ── Test 1: no kill-line → no-op ─────────────────────────────────────

    #[test]
    fn no_kill_line_is_noop() {
        // Pool: heal (no target cast) + reposition (move only).
        // Neither plan is offensive vs target, so strength = None → gate is a no-op.
        let target = ent(2);
        let plans = vec![empty_plan(), move_plan()];
        let raw = vec![
            factors_with(0.0, 0.0), // heal (no damage, no kill)
            factors_with(0.0, 0.0), // reposition
        ];
        let mut scores = vec![0.8, 0.5];
        let modes = default_modes(2);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::None);
        assert!(!stats.applied);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(scores, vec![0.8, 0.5], "scores must be unchanged");
    }

    // ── Test 2: pressure tier prunes non-offensive ───────────────────────

    #[test]
    fn pressure_tier_prunes_non_offensive_only() {
        // Plan A: Cast at intent target, damage = 0.5 * hp (= 10 ≥ 20 * 0.3 = 6), kn=0.
        // Plan B: heal (no cast at target).
        // Strength = Pressure. B is pruned; A survives.
        let target = ent(2);
        let plans = vec![cast_plan(target), empty_plan()];
        let raw = vec![
            factors_with(10.0, 0.0), // offensive, damage ≥ α·hp
            factors_with(0.0, 0.0),  // heal
        ];
        let mut scores = vec![0.6, 0.8];
        let modes = default_modes(2);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::Pressure);
        assert!(stats.applied);
        assert_eq!(stats.pruned_count, 1);
        assert_eq!(scores[0], 0.6, "offensive plan survives");
        assert!(scores[1].is_infinite() && scores[1] < 0.0, "heal pruned to -inf");
    }

    // ── Test 3: can_finish tier prunes all non-killing ───────────────────

    #[test]
    fn can_finish_tier_prunes_all_non_killing() {
        // Plan A: Cast at target, kn=1 (can kill).
        // Plan B: Cast at target, kn=0 (offensive but weak).
        // Plan C: empty (heal / no-cast).
        // Strength = CanFinish. B and C are pruned; A survives.
        let target = ent(2);
        let plans = vec![cast_plan(target), cast_plan(target), empty_plan()];
        let raw = vec![
            factors_with(15.0, 1.0), // killing offensive
            factors_with(8.0, 0.0),  // weak offensive
            factors_with(0.0, 0.0),  // heal
        ];
        let mut scores = vec![0.7, 0.6, 0.9];
        let modes = default_modes(3);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::CanFinish);
        assert!(stats.applied);
        assert_eq!(stats.pruned_count, 2);
        assert_eq!(scores[0], 0.7, "killing plan survives");
        assert!(scores[1].is_infinite() && scores[1] < 0.0, "weak offensive pruned");
        assert!(scores[2].is_infinite() && scores[2] < 0.0, "heal pruned");
    }

    // ── Test 4: regression — collateral kill does NOT raise to CanFinish ──

    #[test]
    fn can_finish_ignores_collateral_kill_line() {
        // Plan A: Cast @ other_enemy (not intent target), kn=1 — collateral kill.
        // Plan B: Cast @ intent target, damage ≥ α·hp, kn=0.
        // Plan C: heal (empty).
        //
        // Intent-coherent detection: A is NOT offensive_vs_target → strength
        // must fall to Pressure (driven by B), NOT CanFinish.
        // Under Pressure: C is pruned, B survives.
        let target = ent(2);
        let other = ent(3);
        let plans = vec![
            cast_plan_at(other, (4, 0)), // Cast @ other_enemy (collateral)
            cast_plan(target),           // Cast @ intent target
            empty_plan(),                // heal
        ];
        let raw = vec![
            factors_with(20.0, 1.0), // collateral kill of other
            factors_with(8.0, 0.0),  // offensive vs target, damage=8 ≥ 20*0.3=6
            factors_with(0.0, 0.0),  // heal
        ];
        let mut scores = vec![0.9, 0.7, 0.8];
        let modes = default_modes(3);
        // Add other_enemy to the snapshot so it's non-empty; target has hp=20
        let target_unit = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .hp(20).max_hp(20).build();
        let other_unit = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 0))
            .hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![target_unit, other_unit], 1);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        // Strength must be Pressure, NOT CanFinish
        assert_eq!(stats.strength, KillLineStrength::Pressure,
            "collateral kill on other enemy must not raise strength to CanFinish");
        // Under Pressure: keep = offensive_vs_target.
        // A (cast at other) and C (heal) are not offensive vs target → both pruned.
        // B (cast at target) survives.
        assert_eq!(stats.pruned_count, 2, "collateral + heal pruned under Pressure");
        assert!(scores[0].is_infinite() && scores[0] < 0.0,
            "collateral plan is not offensive vs target → pruned by Pressure");
        assert_eq!(scores[1], 0.7, "offensive vs target survives Pressure");
        assert!(scores[2].is_infinite() && scores[2] < 0.0, "heal pruned");
    }

    // ── Test 5: collateral damage does not trigger Pressure ──────────────

    #[test]
    fn pressure_ignores_collateral_damage() {
        // Plan A: Cast @ other_enemy (not target), damage=10, kn=0 — collateral.
        // Plan B: empty (heal, no cast at target).
        //
        // Neither plan is offensive_vs_target → strength = None → gate no-op.
        let target = ent(2);
        let other = ent(3);
        let plans = vec![
            cast_plan_at(other, (4, 0)), // Cast @ other, NOT at intent target
            empty_plan(),                // heal
        ];
        let raw = vec![
            factors_with(10.0, 0.0), // collateral damage
            factors_with(0.0, 0.0),  // heal
        ];
        let mut scores = vec![0.7, 0.8];
        let modes = default_modes(2);
        let target_unit = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .hp(20).max_hp(20).build();
        let other_unit = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 0))
            .hp(20).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![target_unit, other_unit], 1);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::None);
        assert!(!stats.applied);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(scores, vec![0.7, 0.8], "no changes");
    }

    // ── Test 6: regression — prior-layer masks respected ─────────────────

    #[test]
    fn gate_ignores_plans_already_masked_by_prior_layer() {
        // Plan A: Cast @ target, kn=1, but ALREADY masked to -inf by prior layer.
        // Plan B: Cast @ target, damage ≥ α·hp, kn=0, score=0.5 (live).
        //
        // Without `.is_finite()` filter: A would be in the live pool →
        // strength=CanFinish → B also pruned → all plans -inf (bad).
        //
        // With fix: A is NOT in live_pool (score=-inf) → strength falls to
        // Pressure (driven by B) → B survives; heal pruned if present.
        let target = ent(2);
        let plans = vec![
            cast_plan(target), // A: masked killing plan
            cast_plan(target), // B: live offensive plan
        ];
        let raw = vec![
            factors_with(20.0, 1.0), // A: would be CanFinish if visible
            factors_with(8.0, 0.0),  // B: damage=8 ≥ 20*0.3=6, Pressure
        ];
        let mut scores = vec![f32::NEG_INFINITY, 0.5]; // A already masked
        let modes = default_modes(2);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        // Masked plan A is invisible → strength driven by B alone → Pressure
        assert_eq!(stats.strength, KillLineStrength::Pressure,
            "already-masked killing plan must not inflate strength to CanFinish");
        assert_eq!(stats.pruned_count, 0, "B is offensive vs target → survives Pressure");
        assert!(scores[0].is_infinite() && scores[0] < 0.0, "A stays at -inf (untouched by gate)");
        assert_eq!(scores[1], 0.5, "B survives");
    }

    // ── Test 7: LastStand mode is excluded from live pool ─────────────────

    #[test]
    fn gate_respects_last_stand_mode() {
        // Plan A: Cast @ target, kn=1, mode=LastStand (adaptation made it lethal).
        // Plan B: defensive / move, mode=Default, score=0.5.
        //
        // A is NOT in the live pool (mode != Default) → strength=None → no-op.
        // B survives untouched.
        let target = ent(2);
        let plans = vec![
            cast_plan(target), // A: killing but self-lethal → LastStand
            move_plan(),       // B: defensive move → Default
        ];
        let raw = vec![
            factors_with(20.0, 1.0), // A: would be CanFinish if visible
            factors_with(0.0, 0.0),  // B: no kill signal
        ];
        let mut scores = vec![0.9, 0.5];
        let modes = vec![EvaluationMode::LastStand, EvaluationMode::Default];
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::FocusTarget { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::None,
            "LastStand plan is invisible → no kill-line detected");
        assert!(!stats.applied);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(scores[1], 0.5, "defensive plan survives");
    }

    // ── Test 8: gate disabled under ApplyCC ──────────────────────────────

    #[test]
    fn gate_disabled_under_apply_cc() {
        // Intent is ApplyCC (not FocusTarget) → early return, no pruning.
        let target = ent(2);
        let plans = vec![cast_plan(target), empty_plan()];
        let raw = vec![
            factors_with(20.0, 1.0),
            factors_with(0.0, 0.0),
        ];
        let mut scores = vec![0.7, 0.9];
        let modes = default_modes(2);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::ApplyCC { target };

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::None);
        assert!(!stats.applied);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(scores, vec![0.7, 0.9], "ApplyCC is not FocusTarget → no-op");
    }

    // ── Test 9: gate disabled under ProtectSelf (guard test) ─────────────

    #[test]
    fn gate_disabled_under_protect_self() {
        // Intent is ProtectSelf → the `FocusTarget` destructure fails → early return.
        // The caller in ranking.rs is also guarded, but we verify the function
        // itself returns the right no-op stats when called with ProtectSelf.
        let plans = vec![empty_plan()];
        let raw = vec![factors_with(20.0, 1.0)];
        let mut scores = vec![0.8];
        let modes = default_modes(1);
        let snap = snap_with_target(2, 20);
        let intent = TacticalIntent::ProtectSelf;

        let stats = apply_killable_gate(&plans, &raw, &mut scores, &modes, &intent, &snap);

        assert_eq!(stats.strength, KillLineStrength::None);
        assert!(!stats.applied);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(scores, vec![0.8], "ProtectSelf is not FocusTarget → no-op");
    }
}
