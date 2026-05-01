//! Terminal factor enum and per-variant leaf modules.
//!
//! `TerminalFactor` covers 8 one-shot per-plan axes derived from the final sim
//! snapshot. All compute sigs are `(plan, snap, ctx)` for uniformity; bodies
//! that don't need `snap` bind it as `_snap`.
//!
//! The new `TerminalScore` typed wrapper lives here (distinct from the legacy
//! `planning::terminal::TerminalScore`). Both coexist in commit 1.

pub mod ally_rescue;
pub mod board_control_gain;
pub mod density_value;
pub mod exposure_at_end;
pub mod line_actionability;
pub mod next_turn_lethality;
pub mod pressure_spacing_zone;
pub mod secure_kill;

use crate::combat::ai::scoring::factors::registry::{default_norm, BatchStats, NeedAxis};
use crate::combat::ai::planning::types::TurnPlan;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::utility::ScoringCtx;

crate::factor_kind! {
    name: TerminalFactor,
    variants: [
        ExposureAtEnd,
        NextTurnLethality,
        SecureKill,
        AllyRescue,
        BoardControlGain,
        LineActionability,
        DensityValue,
        PressureSpacingZone,
    ]
}

impl TerminalFactor {
    pub fn name(self) -> &'static str {
        match self {
            Self::ExposureAtEnd       => "exposure_at_end",
            Self::NextTurnLethality   => "next_turn_lethality",
            Self::SecureKill          => "secure_kill",
            Self::AllyRescue          => "ally_rescue",
            Self::BoardControlGain    => "board_control_gain",
            Self::LineActionability   => "line_actionability",
            Self::DensityValue        => "density_value",
            Self::PressureSpacingZone => "pressure_spacing_zone",
        }
    }

    /// All terminal factors are non-negative (unsigned normalisation).
    pub fn signed(self) -> bool {
        false
    }

    pub fn normalize(self, raw: f32, batch: &BatchStats) -> f32 {
        default_norm(raw, batch, self.signed())
    }

    /// Which `NeedSignals` axis modulates this terminal factor.
    pub fn need_modulation(self) -> NeedAxis {
        match self {
            Self::ExposureAtEnd       => NeedAxis::SelfPreserve,
            Self::NextTurnLethality   => NeedAxis::SelfPreserve,
            Self::SecureKill          => NeedAxis::FinishTarget,
            Self::AllyRescue          => NeedAxis::RescueAlly,
            Self::BoardControlGain    => NeedAxis::Reposition,
            Self::LineActionability   => NeedAxis::None,
            Self::DensityValue        => NeedAxis::SetupAOE,
            Self::PressureSpacingZone => NeedAxis::None,
        }
    }

    pub fn count() -> usize { COUNT }

    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::ExposureAtEnd,
            Self::NextTurnLethality,
            Self::SecureKill,
            Self::AllyRescue,
            Self::BoardControlGain,
            Self::LineActionability,
            Self::DensityValue,
            Self::PressureSpacingZone,
        ]
        .into_iter()
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "exposure_at_end"        => Some(Self::ExposureAtEnd),
            "next_turn_lethality"    => Some(Self::NextTurnLethality),
            "secure_kill"            => Some(Self::SecureKill),
            "ally_rescue"            => Some(Self::AllyRescue),
            "board_control_gain"     => Some(Self::BoardControlGain),
            "line_actionability"     => Some(Self::LineActionability),
            "density_value"          => Some(Self::DensityValue),
            "pressure_spacing_zone"  => Some(Self::PressureSpacingZone),
            _ => None,
        }
    }

    /// Compute this terminal factor.
    ///
    /// `snap` is the initial (pre-plan) battle snapshot. Bodies that don't
    /// need it bind the parameter as `_snap`.
    pub fn compute(self, plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
        match self {
            Self::ExposureAtEnd       => exposure_at_end::compute(plan, snap, ctx),
            Self::NextTurnLethality   => next_turn_lethality::compute(plan, snap, ctx),
            Self::SecureKill          => secure_kill::compute(plan, snap, ctx),
            Self::AllyRescue          => ally_rescue::compute(plan, snap, ctx),
            Self::BoardControlGain    => board_control_gain::compute(plan, snap, ctx),
            Self::LineActionability   => line_actionability::compute(plan, snap, ctx),
            Self::DensityValue        => density_value::compute(plan, snap, ctx),
            Self::PressureSpacingZone => pressure_spacing_zone::compute(plan, snap, ctx),
        }
    }
}

// ── TerminalScore typed wrapper ───────────────────────────────────────────────

/// Typed `[f32; 8]` wrapper for terminal factor values. Indexed by
/// `TerminalFactor`. Distinct from legacy `planning::terminal::TerminalScore`.
///
/// Serialised as a named map `{"exposure_at_end": ..., ...}`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct TerminalScore([f32; 8]);

impl TerminalScore {
    pub fn get(&self, f: TerminalFactor) -> f32 {
        self.0[f as usize]
    }

    pub fn set(&mut self, f: TerminalFactor, v: f32) {
        self.0[f as usize] = v;
    }

    pub fn as_array(&self) -> [f32; 8] {
        self.0
    }
}

impl serde::Serialize for TerminalScore {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = ser.serialize_map(Some(TerminalFactor::count()))?;
        for f in TerminalFactor::iter() {
            map.serialize_entry(f.name(), &self.get(f))?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for TerminalScore {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = TerminalScore;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a map of terminal factor name to f32")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<TerminalScore, A::Error> {
                let mut out = TerminalScore::default();
                while let Some(key) = map.next_key::<String>()? {
                    let val: f32 = map.next_value()?;
                    match TerminalFactor::from_name(&key) {
                        Some(f) => out.set(f, val),
                        None => {
                            return Err(serde::de::Error::custom(format!(
                                "unknown terminal factor \"{key}\""
                            )))
                        }
                    }
                }
                Ok(out)
            }
        }
        de.deserialize_map(Visitor)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_score_serde_round_trip_named_map() {
        let mut ts = TerminalScore::default();
        ts.set(TerminalFactor::ExposureAtEnd, 0.3);
        ts.set(TerminalFactor::NextTurnLethality, 0.7);
        ts.set(TerminalFactor::SecureKill, 1.0);
        ts.set(TerminalFactor::AllyRescue, 0.5);
        ts.set(TerminalFactor::BoardControlGain, -0.2);
        ts.set(TerminalFactor::LineActionability, 0.4);
        ts.set(TerminalFactor::DensityValue, 0.6);
        ts.set(TerminalFactor::PressureSpacingZone, 0.1);

        let json = serde_json::to_string(&ts).unwrap();
        assert!(json.contains("\"exposure_at_end\""), "expected named map: {json}");
        assert!(json.contains("\"pressure_spacing_zone\""), "missing key: {json}");

        let ts2: TerminalScore = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, ts2, "round-trip mismatch");
    }

    #[test]
    fn terminal_score_missing_keys_default_zero() {
        let json = r#"{"exposure_at_end": 0.5}"#;
        let ts: TerminalScore = serde_json::from_str(json).unwrap();
        assert!((ts.get(TerminalFactor::ExposureAtEnd) - 0.5).abs() < f32::EPSILON);
        assert_eq!(ts.get(TerminalFactor::SecureKill), 0.0);
    }

    #[test]
    fn terminal_score_unknown_key_errors() {
        let json = r#"{"exposure_at_end": 0.5, "nonexistent_factor": 1.0}"#;
        let result: Result<TerminalScore, _> = serde_json::from_str(json);
        assert!(result.is_err(), "unknown key should produce an error");
    }
}
