//! Pure serialization helpers for the engine trace (Phase 5).
//!
//! **No file I/O. No Bevy types.**
//!
//! Each combat produces a JSONL trace file with two record types:
//! - One `InitLine` at combat start (seed, initial units, content fingerprint).
//! - One `StepLine` per `step()` call (action, resulting events, RNG canary,
//!   post-step state hash).
//!
//! This module is callable from engine-crate tests without pulling in Bevy.
//!
//! # Schema version
//!
//! [`SCHEMA_VERSION`] is `48`. Any engine change that adds/removes RNG calls
//! or changes the trace record shape MUST bump this constant (Phase 5 D2).

use serde::{Deserialize, Serialize};

use crate::{
    action::Action,
    content_hash,
    event::Event,
    state::{CombatState, RoundPhase, Unit},
    turn_queue::TurnQueue,
};

/// Trace schema version. Bump on any change that adds/removes RNG calls or
/// alters the trace record shape; most bumps are a clean break with older
/// traces (additive ones note `#[serde(default)]` back-compat at the field).
pub const SCHEMA_VERSION: u32 = 50;

// ── Record types ─────────────────────────────────────────────────────────────

/// First line of a trace file — written once at combat start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitLine {
    pub schema: u32,
    /// Fight folder name = `session_id` (D11). Shared with `ai.jsonl` header
    /// so external tools can join both files by `(session_id, step_range)`.
    pub session_id: String,
    pub rng_seed: u64,
    pub units: Vec<Unit>,
    pub next_synthetic_uid: u64,
    /// Round number at combat start (needed to reconstruct `CombatState` for replay).
    pub round: u32,
    /// Round phase at combat start.
    pub phase: RoundPhase,
    /// Turn queue at combat start (order + cursor index).
    pub turn_queue: TurnQueue,
    pub content_hash: String,
}

/// One line per `step()` call — written immediately after each action resolves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepLine {
    pub schema: u32,
    pub step: u64,
    pub action: Action,
    pub events: Vec<Event>,
    /// Number of RNG calls consumed by this step.
    pub rng_calls: u64,
    /// BLAKE3 hash of engine state after this step (canary for mid-trace drift).
    pub post_state_hash: String,
}

// ── Serialization ─────────────────────────────────────────────────────────────

/// Serialize an `InitLine` to a single JSON string (no trailing newline).
pub fn serialize_init(line: &InitLine) -> serde_json::Result<String> {
    serde_json::to_string(line)
}

/// Serialize a `StepLine` to a single JSON string (no trailing newline).
pub fn serialize_step(line: &StepLine) -> serde_json::Result<String> {
    serde_json::to_string(line)
}

// ── Deserialization ───────────────────────────────────────────────────────────

/// Parse a JSONL line into an `InitLine`.
pub fn parse_init(s: &str) -> serde_json::Result<InitLine> {
    serde_json::from_str(s)
}

/// Parse a JSONL line into a `StepLine`.
pub fn parse_step(s: &str) -> serde_json::Result<StepLine> {
    serde_json::from_str(s)
}

// ── State hash ────────────────────────────────────────────────────────────────

/// Intermediate struct for deterministic state hashing.
///
/// Serialized to JSON for the BLAKE3 input — alive units sorted by id,
/// turn queue, round, phase.
#[derive(Serialize)]
struct StateSnapshot<'a> {
    round: u32,
    phase: RoundPhase,
    turn_queue: &'a TurnQueue,
    alive_units: Vec<&'a Unit>,
}

/// Compute a BLAKE3 hash over the canonical serialization of `state`.
///
/// Covers: `round`, `phase`, `turn_queue`, and alive units sorted by id.
/// Returns the 32-byte digest.
pub fn post_state_hash(state: &CombatState) -> [u8; 32] {
    let mut alive: Vec<&Unit> = state.alive_units().collect();
    // Sort by id for deterministic ordering regardless of Vec insertion order.
    alive.sort_by_key(|u| u.id);

    let snapshot = StateSnapshot {
        round: state.round,
        phase: state.phase,
        turn_queue: &state.turn_queue,
        alive_units: alive,
    };

    let json = serde_json::to_string(&snapshot).expect("StateSnapshot serialization is infallible");

    let mut hasher = blake3::Hasher::new();
    hasher.update(json.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Format the `post_state_hash` digest as a `blake3:<hex>` string.
pub fn post_state_hash_hex(state: &CombatState) -> String {
    content_hash::format_hex(&post_state_hash(state))
}
