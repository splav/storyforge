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
//! [`SCHEMA_VERSION`] is `37`. Any engine change that adds/removes RNG calls
//! or changes the trace record shape MUST bump this constant (Phase 5 D2).

use serde::{Deserialize, Serialize};

use crate::{
    action::Action,
    content_hash,
    event::Event,
    state::{CombatState, RoundPhase, Unit},
    turn_queue::TurnQueue,
};

/// Trace schema version.  Matches the AI-log `SCHEMA_VERSION` after 5f.
/// Bump on any change that adds/removes RNG calls or modifies record shape.
/// v39: `Event::ManaRegenerated` is now also emitted after `Effect::PayCost`
/// for mana-cost casts, replacing the bridge-side mana-diff snapshot approach.
/// Cast streams that previously had a trailing `ManaChanged` entry now carry it
/// inline. Old v38 logs are incompatible (clean break).
/// v40: Two engine wire-shape changes consolidated in one SCHEMA jump:
/// (1) `Event::DotDamaged` atomic variant added (Phase A-S5 in `5db559d`):
///     replaces the legacy `(StatusTicked, UnitDamaged)` pair for damaging
///     DoT ticks. Buff-status ticks (zero damage) still emit `StatusTicked`.
/// (2) `Event::TurnEnded` gains `cause: TurnEndCause` field (Phase B-γ / S6
///     in `4b4b0e3`). Engine emits `TurnEnded{cause: ResourcesExhausted}`
///     inline after a Cast that leaves AP=0 and MP=0, removing the bridge's
///     separate auto-end step.
/// Old v39 traces are incompatible (clean break).
/// v41: Phase C-4 — `Event::PoolChanged` introduced as unified pool-change
/// event surface, dual-emitted alongside legacy `ManaRegenerated`/
/// `EnergyRegenerated`/`RageGained`. AP/MP refill at turn-start now emits
/// `PoolChanged{cause: Refill}` (previously silent). Subsumes S7 from
/// engine-migration.md — `EnergySpent` is expressed as
/// `PoolChanged{pool: Energy, cause: Spent}` instead of a dedicated event.
/// Old v40 traces incompatible (clean break).
pub const SCHEMA_VERSION: u32 = 41;

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
    // populated by step() in 5b
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

    let json = serde_json::to_string(&snapshot)
        .expect("StateSnapshot serialization is infallible");

    let mut hasher = blake3::Hasher::new();
    hasher.update(json.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Format the `post_state_hash` digest as a `blake3:<hex>` string.
pub fn post_state_hash_hex(state: &CombatState) -> String {
    content_hash::format_hex(&post_state_hash(state))
}
