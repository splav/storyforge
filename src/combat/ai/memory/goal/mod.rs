//! Goal data + lifecycle helpers — sub-module of `memory/`.
//!
//! - `context`   — `GoalKind`, `StoredGoalContext`, `extract_goal_context`
//!   (formerly `repair/goal.rs`).
//! - `lifecycle` — `pre_tick` / `post_tick` free functions called by the
//!   orchestrator (formerly `repair/lifecycle.rs`).

pub mod context;
pub mod lifecycle;

pub use context::{GoalKind, StoredGoalContext, extract_goal_context};
pub use lifecycle::{pre_tick, post_tick};
