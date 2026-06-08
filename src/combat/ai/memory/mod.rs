//! Between-tick AI state — memory structures and goal lifecycle.
//!
//! Sub-modules:
//! - `ai_memory`  — `AiMemory` (ECS Component) + `PlanSnapshot` (snapshot for
//!   plan continuation checks). Formerly `intent/memory.rs`.
//! - `goal/`      — `GoalKind`, `StoredGoalContext`, `extract_goal_context`
//!   (formerly `repair/goal.rs`) + `pre_tick`/`post_tick` lifecycle helpers
//!   (formerly `repair/lifecycle.rs`).

pub mod ai_memory;
pub mod goal;

pub use ai_memory::{status_hash, status_hash_engine, AiMemory, PlanSnapshot};
pub use goal::{extract_goal_context, GoalKind, StoredGoalContext};
pub use goal::{post_tick, pre_tick};
