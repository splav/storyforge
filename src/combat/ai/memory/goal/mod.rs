//! Goal data + lifecycle helpers — sub-module of `memory/`.
//!
//! - `context`   — `GoalKind`, `StoredGoalContext`, `extract_goal_context`.
//! - `lifecycle` — `pre_tick` / `post_tick` free functions called by the
//!   orchestrator.

pub mod context;
pub mod lifecycle;

pub use context::{extract_goal_context, GoalKind, StoredGoalContext};
pub use lifecycle::{post_tick, pre_tick};
