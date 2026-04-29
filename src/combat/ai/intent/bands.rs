//! Bands — step 11.1.
//!
//! `PriorityBand` is the top-level routing token that governs which agenda
//! items the AI should build (11.2) and how many (11.4).  In 11.1 the band
//! is **computed but not used**: `assign_band` is called in `pick_action`
//! and the result is immediately discarded with an explicit `let _ = ...` so
//! future reviewers can see the intent.
//!
//! Routing / agenda / per-item scoring land in 11.2–11.4.
//! Log schema bump (PlanAnnotation.band) lands in 11.6.

use bevy::prelude::Entity;
use serde::{Deserialize, Serialize};

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::tuning::AiTuning;

// ── PriorityBand ─────────────────────────────────────────────────────────────

/// The top-level priority band for an actor's turn.
///
/// Evaluated in order; first match wins (Forced → Critical → HardRescue → Normal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityBand {
    /// An enemy with `FORCES_TARGETING` is alive — all actions must target it.
    ForcedTargeting,
    /// Actor is critically wounded in high danger — survival is non-negotiable.
    CriticalSelfPreservation,
    /// A teammate urgently needs rescue and the actor has healing capability.
    HardRescueOpportunity,
    /// Default tactical mode: FocusTarget / ApplyCC / SetupAOE / Reposition.
    NormalTactical,
}

// ── BandWeights ──────────────────────────────────────────────────────────────

/// Per-band weights for the six `IntentConsiderations` axes (introduced in 11.3).
///
/// Stored here alongside the band enum so the source of truth is co-located.
/// Consumed in 11.3 when `IntentConsiderations` scoring lands.
#[derive(Clone, Copy, Debug)]
pub struct BandWeights {
    pub urgency: f32,
    pub feasibility: f32,
    pub leverage: f32,
    pub safety: f32,
    pub role_affinity: f32,
    pub continuation_value: f32,
}

impl PriorityBand {
    /// Hardcoded per-band axis weights.  Calibration rationale:
    ///
    /// - `ForcedTargeting`: maximise feasibility (can we actually hit the taunter?)
    ///   and urgency (we have no choice); leverage and safety are secondary.
    /// - `CriticalSelfPreservation`: safety is dominant; leverage near-zero.
    /// - `HardRescueOpportunity`: leverage (will the heal matter?) + urgency;
    ///   continuation penalised so the actor doesn't stick to a stale rescue plan.
    /// - `NormalTactical`: balanced; role_affinity and continuation elevated.
    pub fn weights(self) -> BandWeights {
        match self {
            PriorityBand::ForcedTargeting => BandWeights {
                urgency:            0.8,
                feasibility:        1.0,
                leverage:           0.5,
                safety:             0.3,
                role_affinity:      0.4,
                continuation_value: 0.2,
            },
            PriorityBand::CriticalSelfPreservation => BandWeights {
                urgency:            1.0,
                feasibility:        0.7,
                leverage:           0.1,
                safety:             1.0,
                role_affinity:      0.2,
                continuation_value: 0.1,
            },
            PriorityBand::HardRescueOpportunity => BandWeights {
                urgency:            0.8,
                feasibility:        0.7,
                leverage:           0.9,
                safety:             0.4,
                role_affinity:      0.6,
                continuation_value: 0.3,
            },
            PriorityBand::NormalTactical => BandWeights {
                urgency:            0.6,
                feasibility:        0.7,
                leverage:           0.7,
                safety:             0.5,
                role_affinity:      0.8,
                continuation_value: 0.7,
            },
        }
    }
}

// ── BandReason ───────────────────────────────────────────────────────────────

/// Diagnostic payload explaining which rule triggered the band assignment.
///
/// Tagged serde enum so the log diff tool can parse it in 11.6.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BandReason {
    /// `ForcedTargeting`: actor is taunted by an enemy with `FORCES_TARGETING`.
    TauntForced { taunter: Entity },
    /// `CriticalSelfPreservation`: panic gate triggered.
    PanicOverride { self_preserve: f32, danger: f32 },
    /// `HardRescueOpportunity`: rescue need exceeds hard threshold.
    HardRescueNeed { rescue_need: f32 },
    /// `NormalTactical`: fallback — no specific trigger.
    Normal,
}

// ── assign_band ──────────────────────────────────────────────────────────────

/// Compute the priority band for `active` given the current battle state.
///
/// Evaluation order (first match wins):
/// 1. `ForcedTargeting` — an enemy with `FORCES_TARGETING` is alive.
/// 2. `CriticalSelfPreservation` — panic gate: same thresholds as
///    `select_intent`'s PanicOverride branch.
/// 3. `HardRescueOpportunity` — `rescue_ally ≥ hard_rescue_threshold` AND the
///    actor has `CAN_HEAL`.
/// 4. `NormalTactical` — fallback.
///
/// **11.1 contract**: result is discarded in `pick_action` — no routing change.
pub fn assign_band(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    needs: &NeedSignals,
    difficulty: &DifficultyProfile,
    tuning: &AiTuning,
) -> (PriorityBand, BandReason) {
    // 1. ForcedTargeting — hard taunt rule.
    if let Some(taunter) = snap
        .enemies_of(active.team)
        .find(|e| e.tags.contains(AiTags::FORCES_TARGETING))
    {
        return (
            PriorityBand::ForcedTargeting,
            BandReason::TauntForced { taunter: taunter.entity },
        );
    }

    // 2. CriticalSelfPreservation — exact same gate as select_intent PanicOverride.
    let danger = maps.danger.get(active.pos);
    let panic_threshold = tuning.thresholds.panic_self_preserve_threshold;
    let danger_panic = difficulty.awareness_danger_threshold(tuning);
    if needs.self_preserve >= panic_threshold && danger > danger_panic {
        return (
            PriorityBand::CriticalSelfPreservation,
            BandReason::PanicOverride {
                self_preserve: needs.self_preserve,
                danger,
            },
        );
    }

    // 3. HardRescueOpportunity — rescue need above hard threshold AND actor can heal.
    // CAN_RESCUE does not exist in this codebase; CAN_HEAL is the sole gate.
    let hard_rescue = tuning.thresholds.hard_rescue_threshold;
    if needs.rescue_ally >= hard_rescue && active.tags.contains(AiTags::CAN_HEAL) {
        return (
            PriorityBand::HardRescueOpportunity,
            BandReason::HardRescueNeed { rescue_need: needs.rescue_ally },
        );
    }

    // 4. NormalTactical — default.
    (PriorityBand::NormalTactical, BandReason::Normal)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::NeedSignals;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::test_helpers::{empty_maps, UnitBuilder};
    use crate::combat::ai::tuning::AiTuning;
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn origin() -> crate::game::hex::Hex {
        hex_from_offset(0, 0)
    }

    fn default_tuning() -> AiTuning {
        AiTuning::default()
    }

    fn default_difficulty() -> DifficultyProfile {
        DifficultyProfile::default()
    }

    fn zero_needs() -> NeedSignals {
        NeedSignals::default()
    }

    // ── 1. ForcedTargeting fires on canonical taunter ─────────────────────

    #[test]
    fn band_forced_targeting_fires_on_canonical_case() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).build();
        let taunter = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .tags(AiTags::FORCES_TARGETING)
            .build();
        let taunter_entity = taunter.entity;
        let snap = BattleSnapshot::new(vec![active.clone(), taunter], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();

        let (band, reason) = assign_band(&active, &snap, &maps, &zero_needs(), &difficulty, &tuning);

        assert_eq!(band, PriorityBand::ForcedTargeting);
        assert_eq!(reason, BandReason::TauntForced { taunter: taunter_entity });
    }

    // ── 2. CriticalSelfPreservation fires on panic conditions ─────────────

    #[test]
    fn band_critical_self_preservation_fires_on_panic() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).hp(2).max_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0)).build();
        let snap = BattleSnapshot::new(vec![active.clone(), enemy], 1);

        let tuning = default_tuning();
        let difficulty = default_difficulty();

        // Build maps with danger above the panic threshold.
        let danger_panic = difficulty.awareness_danger_threshold(&tuning);
        let mut maps = empty_maps();
        maps.danger.add(origin(), danger_panic + 0.1);

        // Drive self_preserve above panic threshold.
        let needs = NeedSignals {
            self_preserve: tuning.thresholds.panic_self_preserve_threshold + 0.01,
            ..NeedSignals::default()
        };

        let (band, reason) = assign_band(&active, &snap, &maps, &needs, &difficulty, &tuning);

        assert_eq!(band, PriorityBand::CriticalSelfPreservation);
        match reason {
            BandReason::PanicOverride { self_preserve, danger } => {
                assert!(self_preserve >= tuning.thresholds.panic_self_preserve_threshold);
                assert!(danger > danger_panic);
            }
            other => panic!("expected PanicOverride, got {other:?}"),
        }
    }

    // ── 3. HardRescueOpportunity fires on high rescue need + CAN_HEAL ─────

    #[test]
    fn band_hard_rescue_opportunity_fires_on_high_rescue_need() {
        let active = UnitBuilder::new(1, Team::Enemy, origin())
            .tags(AiTags::CAN_HEAL)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .hp(1)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![active.clone(), ally], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();

        let needs = NeedSignals {
            rescue_ally: tuning.thresholds.hard_rescue_threshold + 0.05,
            ..NeedSignals::default()
        };

        let (band, reason) = assign_band(&active, &snap, &maps, &needs, &difficulty, &tuning);

        assert_eq!(band, PriorityBand::HardRescueOpportunity);
        match reason {
            BandReason::HardRescueNeed { rescue_need } => {
                assert!(rescue_need >= tuning.thresholds.hard_rescue_threshold);
            }
            other => panic!("expected HardRescueNeed, got {other:?}"),
        }
    }

    // ── 4. NormalTactical is the fallback in a clean state ────────────────

    #[test]
    fn band_normal_tactical_fallback() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).build();
        let snap = BattleSnapshot::new(vec![active.clone(), enemy], 1);
        let maps = empty_maps(); // no danger
        let tuning = default_tuning();
        let difficulty = default_difficulty();
        // Zero needs — no panic, no rescue pressure.
        let needs = zero_needs();

        let (band, reason) = assign_band(&active, &snap, &maps, &needs, &difficulty, &tuning);

        assert_eq!(band, PriorityBand::NormalTactical);
        assert_eq!(reason, BandReason::Normal);
    }

    // ── 5. Priority order: ForcedTargeting beats CriticalSelfPreservation ─

    #[test]
    fn band_priority_order_forced_beats_critical() {
        // Actor is both taunted AND in a panic state — Forced must win.
        let active = UnitBuilder::new(1, Team::Enemy, origin()).hp(2).max_hp(20).build();
        let taunter = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .tags(AiTags::FORCES_TARGETING)
            .build();
        let taunter_entity = taunter.entity;
        let snap = BattleSnapshot::new(vec![active.clone(), taunter], 1);

        let tuning = default_tuning();
        let difficulty = default_difficulty();

        // Set danger above panic threshold.
        let danger_panic = difficulty.awareness_danger_threshold(&tuning);
        let mut maps = empty_maps();
        maps.danger.add(origin(), danger_panic + 0.1);

        // self_preserve above panic threshold.
        let needs = NeedSignals {
            self_preserve: tuning.thresholds.panic_self_preserve_threshold + 0.01,
            rescue_ally: tuning.thresholds.hard_rescue_threshold + 0.05,
            ..NeedSignals::default()
        };

        let (band, reason) = assign_band(&active, &snap, &maps, &needs, &difficulty, &tuning);

        assert_eq!(band, PriorityBand::ForcedTargeting, "Forced must beat Critical+HardRescue");
        assert_eq!(reason, BandReason::TauntForced { taunter: taunter_entity });
    }
}
