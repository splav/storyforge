//! # `combat_engine` — pure-Rust combat state machine
//!
//! See `docs/ai/rework/unisim.md` §2.1–2.6 for architecture rationale.
//!
//! **Zero `bevy::` imports anywhere in this tree.**  All Bevy glue lives in
//! `storyforge::combat::engine_bridge`.
//!
//! ## Concept hierarchy
//! - **Action** — coarse player/AI intent (`Move`, `Cast`, …).
//! - **Effect** — atomic state mutation produced by the engine.
//! - **Event** — observable fact emitted for UI/log/replay consumers.
//!
//! ## Entry point
//! ```ignore
//! let events = combat_engine::step(&mut state, action, &mut rng, &content)?;
//! ```

// ── StatusId ──────────────────────────────────────────────────────────────────

/// Macro to create a newtype string id with standard trait impls.
/// Mirrors `storyforge::core::string_id!` — kept in sync manually.
macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                $name(s)
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

string_id!(AbilityId);
string_id!(ArmorId);
string_id!(StatusId);
string_id!(WeaponId);

// ── ResourceKind ──────────────────────────────────────────────────────────────

/// Вид ресурса, который может тратиться на способности.
///
/// Mirrors `storyforge::core::ResourceKind` — kept in sync manually.
/// `storyforge::core` re-exports this type directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ResourceKind {
    Hp,
    Mana,
    Rage,
    Energy,
}

pub mod action;
pub mod content;
pub mod dice;
pub mod effect;
pub mod event;
pub mod reaction;
pub mod state;
pub mod step;

pub use dice::{DiceExpr, DiceRng};
pub use content::{AbilityDef, AbilityRange, Cost, StatusDef, TargetType};
