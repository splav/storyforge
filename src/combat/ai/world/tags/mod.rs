//! AI semantic tags for abilities, statuses, and per-unit combat state.
//!
//! Tags are *derived* projections of ability/status shape, computed once at
//! content load time and cached. The classifier is pure: same shape → same
//! tags. See `docs/ai_rework_step9_plan.md` for the full spec.
//!
//! Sub-modules:
//! - `cache`    — `AbilityTagCache`, `StatusTagCache`.
//! - `classify` — tag derivation from content shapes.
//! - `ai_tags`  — `AiTags` bitflags (formerly in `world/snapshot.rs`; moved R7).

pub mod ai_tags;
pub mod cache;
pub mod classify;

pub use ai_tags::AiTags;
pub use cache::{AbilityTagCache, StatusBonuses, StatusTagCache};
pub use classify::{derive_ability_tags, derive_status_tags, StatusTagLookup};

use serde::{Deserialize, Serialize};

// ── Ability tags ─────────────────────────────────────────────────────────────

/// Closed enum of derivable ability semantics. 7 variants.
/// See `docs/ai_rework_step9_plan.md` §9 for the full classification rules.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbilityTag {
    Offensive,
    Defensive,
    Rescue,
    Summon,
    Mobility,
    ApplyCC,
    Peel,
}

impl AbilityTag {
    pub fn name(self) -> &'static str {
        match self {
            Self::Offensive => "offensive",
            Self::Defensive => "defensive",
            Self::Rescue => "rescue",
            Self::Summon => "summon",
            Self::Mobility => "mobility",
            Self::ApplyCC => "apply_cc",
            Self::Peel => "peel",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "offensive" => Some(Self::Offensive),
            "defensive" => Some(Self::Defensive),
            "rescue" => Some(Self::Rescue),
            "summon" => Some(Self::Summon),
            "mobility" => Some(Self::Mobility),
            "apply_cc" => Some(Self::ApplyCC),
            "peel" => Some(Self::Peel),
            _ => None,
        }
    }

    /// Iteration order = bitset write order = JSON list order.
    /// Pinned by `ability_tag_set_iter_order_is_stable`.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Offensive,
            Self::Defensive,
            Self::Rescue,
            Self::Summon,
            Self::Mobility,
            Self::ApplyCC,
            Self::Peel,
        ]
        .into_iter()
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct AbilityTagSet: u8 {
        const OFFENSIVE = 0b0000_0001;
        const DEFENSIVE = 0b0000_0010;
        const RESCUE    = 0b0000_0100;
        const SUMMON    = 0b0000_1000;
        const MOBILITY  = 0b0001_0000;
        const APPLY_CC  = 0b0010_0000;
        const PEEL      = 0b0100_0000;
    }
}

impl AbilityTagSet {
    pub fn from_iter_tags<I: IntoIterator<Item = AbilityTag>>(it: I) -> Self {
        let mut s = Self::empty();
        for t in it {
            s.insert_tag(t);
        }
        s
    }

    pub fn contains_tag(self, t: AbilityTag) -> bool {
        self.contains(Self::tag_bit(t))
    }

    pub fn insert_tag(&mut self, t: AbilityTag) {
        self.insert(Self::tag_bit(t));
    }

    pub fn iter_tags(self) -> impl Iterator<Item = AbilityTag> {
        AbilityTag::iter().filter(move |&t| self.contains_tag(t))
    }

    fn tag_bit(t: AbilityTag) -> Self {
        match t {
            AbilityTag::Offensive => Self::OFFENSIVE,
            AbilityTag::Defensive => Self::DEFENSIVE,
            AbilityTag::Rescue => Self::RESCUE,
            AbilityTag::Summon => Self::SUMMON,
            AbilityTag::Mobility => Self::MOBILITY,
            AbilityTag::ApplyCC => Self::APPLY_CC,
            AbilityTag::Peel => Self::PEEL,
        }
    }
}

// Manual Serialize/Deserialize as Vec<&'static str> in iter() order —
// keeps log diffs reviewable and avoids dependency on bitflags serde feature.
impl Serialize for AbilityTagSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let tags: Vec<&'static str> = self.iter_tags().map(|t| t.name()).collect();
        let mut seq = s.serialize_seq(Some(tags.len()))?;
        for name in tags {
            seq.serialize_element(name)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for AbilityTagSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names: Vec<String> = Vec::deserialize(d)?;
        let mut set = Self::empty();
        for n in names {
            let t = AbilityTag::from_name(&n)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown ability tag '{n}'")))?;
            set.insert_tag(t);
        }
        Ok(set)
    }
}

// ── Status tags ───────────────────────────────────────────────────────────────

/// Closed enum of derivable status semantics. 5 variants.
///
/// Note: `forces_targeting` (taunted-style) is NOT a StatusTag — it's a raw
/// shape flag checked directly from `StatusDef` when classifying Peel at the
/// ability level. This cleanly separates "AI semantic" (5 tags) from "raw shape
/// flag" (forces_targeting).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusTag {
    HardCC,
    SoftCC,
    Dot,
    Buff,
    /// Step 9.B: derived from `forces_targeting = true` (taunted-like statuses).
    /// Set in parallel with other tags; replaces the Cosmetic fallback when it
    /// is the sole distinguishing property.
    /// Treated as Invalidating by `repair::classify_mismatch` (commit 3).
    Compulsion,
    Cosmetic,
}

impl StatusTag {
    pub fn name(self) -> &'static str {
        match self {
            Self::HardCC => "hard_cc",
            Self::SoftCC => "soft_cc",
            Self::Dot => "dot",
            Self::Buff => "buff",
            Self::Compulsion => "compulsion",
            Self::Cosmetic => "cosmetic",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "hard_cc" => Some(Self::HardCC),
            "soft_cc" => Some(Self::SoftCC),
            "dot" => Some(Self::Dot),
            "buff" => Some(Self::Buff),
            "compulsion" => Some(Self::Compulsion),
            "cosmetic" => Some(Self::Cosmetic),
            _ => None,
        }
    }

    /// Iteration order = bitset write order = JSON list order.
    /// Pinned by `status_tag_set_iter_order_is_stable`.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::HardCC,
            Self::SoftCC,
            Self::Dot,
            Self::Buff,
            Self::Compulsion,
            Self::Cosmetic,
        ]
        .into_iter()
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct StatusTagSet: u8 {
        const HARD_CC   = 0b0000_0001;
        const SOFT_CC   = 0b0000_0010;
        const DOT       = 0b0000_0100;
        const BUFF      = 0b0000_1000;
        /// Step 9.B: set when `forces_targeting = true` on the status def.
        const COMPULSION = 0b0010_0000;
        const COSMETIC  = 0b0001_0000;
    }
}

impl StatusTagSet {
    pub fn from_iter_tags<I: IntoIterator<Item = StatusTag>>(it: I) -> Self {
        let mut s = Self::empty();
        for t in it {
            s.insert_tag(t);
        }
        s
    }

    pub fn contains_tag(self, t: StatusTag) -> bool {
        self.contains(Self::tag_bit(t))
    }

    pub fn insert_tag(&mut self, t: StatusTag) {
        self.insert(Self::tag_bit(t));
    }

    pub fn iter_tags(self) -> impl Iterator<Item = StatusTag> {
        StatusTag::iter().filter(move |&t| self.contains_tag(t))
    }

    fn tag_bit(t: StatusTag) -> Self {
        match t {
            StatusTag::HardCC => Self::HARD_CC,
            StatusTag::SoftCC => Self::SOFT_CC,
            StatusTag::Dot => Self::DOT,
            StatusTag::Buff => Self::BUFF,
            StatusTag::Compulsion => Self::COMPULSION,
            StatusTag::Cosmetic => Self::COSMETIC,
        }
    }
}

impl Serialize for StatusTagSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let tags: Vec<&'static str> = self.iter_tags().map(|t| t.name()).collect();
        let mut seq = s.serialize_seq(Some(tags.len()))?;
        for name in tags {
            seq.serialize_element(name)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for StatusTagSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names: Vec<String> = Vec::deserialize(d)?;
        let mut set = Self::empty();
        for n in names {
            let t = StatusTag::from_name(&n)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown status tag '{n}'")))?;
            set.insert_tag(t);
        }
        Ok(set)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ability_tag_set_iter_order_is_stable() {
        let tags: Vec<AbilityTag> = AbilityTag::iter().collect();
        assert_eq!(
            tags,
            vec![
                AbilityTag::Offensive,
                AbilityTag::Defensive,
                AbilityTag::Rescue,
                AbilityTag::Summon,
                AbilityTag::Mobility,
                AbilityTag::ApplyCC,
                AbilityTag::Peel,
            ]
        );
    }

    #[test]
    fn status_tag_set_iter_order_is_stable() {
        let tags: Vec<StatusTag> = StatusTag::iter().collect();
        assert_eq!(
            tags,
            vec![
                StatusTag::HardCC,
                StatusTag::SoftCC,
                StatusTag::Dot,
                StatusTag::Buff,
                StatusTag::Compulsion,
                StatusTag::Cosmetic,
            ]
        );
    }

    #[test]
    fn ability_tag_set_serde_round_trip_named_list() {
        let set = AbilityTagSet::OFFENSIVE | AbilityTagSet::PEEL | AbilityTagSet::RESCUE;
        let json = serde_json::to_string(&set).unwrap();
        // Must be ["offensive","rescue","peel"] in iter() order
        assert_eq!(json, r#"["offensive","rescue","peel"]"#);
        let decoded: AbilityTagSet = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, set);
    }

    #[test]
    fn status_tag_set_serde_round_trip_named_list() {
        let set = StatusTagSet::HARD_CC | StatusTagSet::DOT;
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["hard_cc","dot"]"#);
        let decoded: StatusTagSet = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, set);
    }

    #[test]
    fn ability_tag_set_unknown_string_in_deserialize_errors() {
        let result: Result<AbilityTagSet, _> = serde_json::from_str(r#"["bogus_tag"]"#);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unknown ability tag"),
            "error message was: {msg}"
        );
    }
}
