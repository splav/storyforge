//! `AiTags` — per-unit bitflags computed from snapshot state.
//!
//! Formerly defined in `world/snapshot.rs`; moved to `world/tags/` (R7)
//! so that all tag semantics live in one place alongside `AbilityTag` and
//! `StatusTag`.

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AiTags: u16 {
        const LOW_HP     = 0b0000_0001;
        const CAN_HEAL   = 0b0000_0010;
        const CAN_CC     = 0b0000_0100;
        const HAS_AOE    = 0b0000_1000;
        // bits 0x10 (IS_STUNNED) and 0x20 (FORCES_TARGETING) removed —
        // those are now lazy methods on UnitView / UnitSnapshot.
        const RANGED     = 0b0100_0000;
        const MELEE_ONLY = 0b1000_0000;
    }
}
