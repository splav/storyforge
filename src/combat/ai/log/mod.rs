//! Structured AI decision log (JSONL) for offline analysis and scoring
//! calibration.
//!
//! # Format
//!
//! One JSON object per line. Each entry records a single `pick_action` call:
//! the full battle snapshot as seen by the AI, the chosen intent, the plan
//! pool (top-N with raw factors and scores), and the committed decision.
//! Deterministic function of the snapshot — no RNG roll logged — so offline
//! replay with different scoring weights reproduces the ranking.
//!
//! `schema_version` starts at **1**. Strict: analyzers must match exact
//! version; schema changes bump the number and require an explicit migration.
//!
//! # Enable
//!
//! `[debug].ai_log = true` in `assets/data/settings.toml`.
//!
//! # Files
//!
//! One file per combat, written into `./logs/` under the current working
//! directory (typically the project root when launched via `cargo run`).
//! Name: `<UTC-timestamp>_<campaign>_<scenario>_<encounter>.jsonl`.
//! Timestamp format: `YYYYMMDDTHHMMSS`. `campaign` is `standalone` when no
//! campaign frame is active.

#![allow(clippy::too_many_arguments)]

pub mod debug;
pub mod serde_helpers;

use std::fs::{create_dir_all, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::intent::{AiMemory, IntentKind, IntentReason, TacticalIntent};
use crate::combat::ai::intent::bands::{BandReason, PriorityBand};
use crate::combat::ai::intent::considerations::IntentConsiderations;
use crate::combat::ai::repair::{ContinuationSeverity, StoredGoalContext};
use crate::combat::ai::memory::goal::GoalKind;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::adapt::{AdaptationReason, EvaluationMode};
use crate::combat::ai::pipeline::stages::sanity::SanityHit;
use crate::combat::ai::plan::{PlanStep, StepOutcome, TurnPlan};
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::orchestration::AiDecision;
use crate::core::AbilityId;
use crate::game::hex::Hex;

/// Log schema version.
/// - v1 → v2: added `reactions_left` + `aoo_expected_damage` to `UnitSnapshot`.
///   v1 logs are still readable via `#[serde(default)]` on the new fields
///   (defaults: `reactions_left=1` — matches the only content-wide `Reactions::max`;
///   `aoo_expected_damage=None` — no damage info available).
/// - v4 → v5: added `intent.reason` — structured `IntentReason` enum dump
///   alongside `selection_kind` + `reason_text`. Lets analyzers read numeric
///   fields (hp_pct, danger, eff_hp, …) directly instead of re-parsing
///   `reason_text`. v4 logs stay readable — the field is optional on read.
/// - v5 → v6: per-plan ADAPTATION dump — `evaluation_mode`,
///   `adaptation_reason`, `base_score` added to `PlanLogEntry`. `score`
///   stays the final (post-adaptation) number so v1-v5 consumers still
///   read a comparable value. Older logs without the new fields deser
///   via `#[serde(default)]` — `evaluation_mode=Default`,
///   `adaptation_reason=None`, `base_score=score`.
/// - v6 → v7: per-plan TRADE block (`trade: { delta, killed, lost,
///   self_lost, self_lethal, score }`) added to `PlanLogEntry`. `score`
///   in the top-level row is still the final number (already includes
///   `trade.score`); the block just exposes the HP-equivalent
///   breakdown that produced the `score` contribution. Older logs
///   tolerate via `#[serde(default)]` — empty breakdown.
/// - v7 → v8: new `AdaptationReason::ProtectSelfFutile { pending_dot,
///   actor_hp }` variant. Older logs don't contain this kind, so they
///   stay readable; analyzers must grow a case for the new `kind` code
///   `"protect_self_futile"` to decode v8 entries that carry it.
/// - v8 → v9: new `event_type = "plan_divergence"` entries emitted when
///   the freeze-after-move logic has both a stored plan and a fresh plan
///   to compare. Separate JSON object alongside the regular `pick_action`
///   entries. Old analyzers see unknown event_type and can skip gracefully.
/// - v9 → v10: `raw_factors` expands from 9 to 10 elements — the new
///   `tempo_gain` axis (index 9) is appended. Old logs deserialized via
///   `Vec<f32>` in the replay tool; the 10th element defaults to 0.0.
/// - v10 → v11: `kill` axis (index 1) is split into `kill_now` (index 1) and
///   `kill_promised` (index 2); all subsequent indices shift by +1.
///   `raw_factors` expands to 11 elements. Old logs treat `kill` as `kill_now`.
/// - v11 → v12: new `saturation` axis (index 11) — buff-redundancy penalty.
///   `raw_factors` expands to 12 elements. Old logs missing the field default to 0.0.
/// - v12 → v13: new `self_survival` axis (index 12) — plan-level defensive value.
///   `raw_factors` expands to 13 elements. Old logs default to 0.0.
/// - v13 → v14: Phase 6 cleanup. Removed `position` (5), `risk` (6), `focus` (7)
///   axes. `raw_factors` shrinks to 10 elements. Indices 5–9 now map to
///   intent/scarcity/tempo_gain/saturation/self_survival. **Not backward-
///   compatible** — old v1–v13 logs cannot be replayed with v14 raw_factors.
/// - v14 → v15: added 4 entry-level telemetry fields for future killable gate
///   (step 3 of the rework): `gate_applied`, `gate_pruned_count`,
///   `survival_mode_active`, `last_stand_active`. v14 logs deserialize via
///   `#[serde(default)]` → false / 0 for new fields. Until step 3 ships,
///   `gate_applied` and `gate_pruned_count` are always stub values
///   (false / 0); `last_stand_active` and `survival_mode_active` are
///   derived at log-time from the plan pool and the intent selection kind.
/// - v15 → v16: new per-plan `sanity_breakdown` field — list of
///   `{rule, multiplier}` objects for sanity rules that fired on that
///   plan (step 0.3C of the rework). `rule` is a snake_case string
///   (e.g. `"survival"`, `"aoo_bleed"`); `multiplier` is the factor
///   actually applied (post-floor clamp). v15 logs deserialize via
///   `#[serde(default)]` → empty vec, preserving backward compatibility.
/// - v16 → v17: three pre-decision snapshots added to `AiLogEntry`:
///   `difficulty` (`DifficultyProfileSnapshot`), `ai_memory`
///   (`Option<AiMemorySnapshot>`), `reservations` (`ReservationsSnapshot`).
///   Makes JSONL self-contained for replay: steps 1.4 "Plan freeze" and
///   "Team coordination" need the real difficulty + memory + reservation
///   state rather than hardcoded defaults. v16 logs deserialize via
///   `#[serde(default)]` on the new fields.
/// - v17 → v18: `UnitSnapshot.ai_tuning_override` added (default `None`).
///   Per-unit AiTuning override scaffolding (step 2.7). v17 logs deserialize
///   via `#[serde(default)]` → `None`, preserving backward compatibility.
/// - v18 → v19: `TurnPlan.annotation` (`PlanAnnotation` with `outcomes` vector)
///   serialized into `PlanLogEntry`. v18 logs deserialize via `#[serde(default)]`
///   → empty annotation, preserving backward compatibility.
/// - v19 → v20: `AiMemorySnapshot` extended with 3 fields
///   (`hp_ratio_at_last_turn`, `last_turn_was_defensive`, `turns_in_low_hp`)
///   for the appraisal / need layer (step 3.0). v19 logs deserialize via
///   `#[serde(default)]` on the new fields, preserving backward compatibility.
/// - v20 → v21: `IntentReason::PanicOverride` fields renamed
///   (`hp_pct` → `self_preserve`, `hp_threshold` → `self_preserve_threshold`);
///   `IntentReason::Urgency` field renamed (`hp_pct` → `self_preserve`).
///   Step 3.2 consumer wiring. Old v20 logs with `hp_pct`/`hp_threshold` fields
///   in those variants will deserialize to 0.0 for the renamed fields (Serde
///   unknown-field drop), which is acceptable for replay/analysis purposes.
/// - v21 → v22: `IntentReason::Reposition` fields renamed
///   (`pos_eval`/`threshold` → `reposition`/`floor`) when select_intent migrated
///   to need_signals (step 3.4). Old v21 logs deserialize via Serde default for
///   unknown fields; the renamed fields get 0.0.
/// - v22 → v23: `PlanAnnotation.terminal` (`TerminalScore`, 8 axes) serialized
///   into `PlanLogEntry`. v22 logs deserialize via `#[serde(default)]` →
///   zero-filled `TerminalScore`, preserving backward compatibility.
/// - v23 → v24 (step 6.5): added `continuation_outcome`, `repair_affinity`,
///   `repair_bonus`, `goal_kind` to `PlanDivergenceEntry`. All fields are
///   `Option` or have `#[serde(default)]` — backward-compat with v23 logs
///   (missing fields → `NoStoredGoal` / `None`).
/// - v24 → v25 (step 6.6): `AiMemorySnapshot.last_plan` (`StoredPlanSnapshot`)
///   replaced by `last_goal` (`Option<StoredGoalContextSnapshot>`).
///   `StoredPlanSnapshot` struct removed. `PlanDivergenceEntry.used_continuation`
///   kept for backward compat (always `false` — exact-continuation removed).
///   v24 logs deserialize with `last_goal = None` via `#[serde(default)]`.
/// - v25 → v26 (step 6.6b metric refinement): `ContinuationOutcome` variants
///   renamed and split. `GoalPreservedMethodPreserved` → `GoalPreservedMethodDelivered`
///   (alias preserved). `GoalPreservedMethodChanged` → `GoalPreservedInTransit`
///   (alias preserved). `GoalAbandoned { reason }` split into four variants:
///   `GoalAbandonedReactive { source }`, `GoalAbandonedVoluntary`,
///   `GoalAbandonedInvalidating`, `GoalAbandonedTtlExpired`. Old `goal_abandoned`
///   kind in v25 logs does not match the new tagged shape; falls through serde
///   to `NoStoredGoal` via `#[serde(other)]` (acceptable for analysis — v25
///   abandoned entries are not split granularly anyway). `AbandonReason` enum
///   removed (merged into outcome variants above).
///
/// v28 (step 4.12, 2026-04-26): `ActionOutcomeEstimate` legacy fields removed
///   (`expected_damage`, `deny_value`, `rescue_value`, `board_pressure`,
///   `geometry_gain`, `exposure_delta`, `resource_swing`). Outcomes now contain
///   fact fields only. v27 logs are incompatible — clean break.
/// Step 9.B: bumped to v30 for `actor_statuses_at_capture` / `actor_statuses_at_store`
/// fields added in commit 3. v29 logs give `LogError::UnsupportedSchema`.
/// Step 10.4: bumped to v31. `PlanAnnotation.critics: Vec<CriticHit>` is now
/// serialised; `SanityRule` enum shrinks to 3 residual variants (Survival /
/// AoOBleed / LosBlindspot / SelfAoe removed). v30 logs give
/// `LogError::UnsupportedSchema` — clean break.
/// Step 11.6: bumped to v32. `ActorTickEvent` gains `band`, `band_reason`, and
/// `agenda` fields; `PlanAnnotation` gains `agenda_item` and
/// `considerations_per_item`. v31 logs pre-date bands/agenda serialisation and
/// give `LogError::UnsupportedSchema` — clean break.
/// P3b: bumped to v33. `PlanAnnotation` gains `score_trace_log`
/// (schema-additive; v32 logs without this field deserialise as `None`).
/// P7: bumped to v34. `ActorTickEvent` gains `evaluation_mode_reason`
/// (schema-additive; v33 logs without this field deserialise as `None`).
/// `IntentBlock` gains `evaluation_mode_reason` (schema-additive).
/// `TacticalIntent::LastStand` removed — was never emitted by `select_intent`.
/// `IntentReason::Adapted` removed — replaced by parallel `evaluation_mode_reason` field.
pub const SCHEMA_VERSION: u32 = 34;

/// Bevy resource owning the log writer. Absent / `None` writer = logging off.
/// Plan id counter is kept even when writer is off so analysis tools can
/// relate manual console traces by id if one is attached mid-session.
#[derive(Resource, Default)]
pub struct AiLogger {
    writer: Option<BufWriter<File>>,
    plan_counter: u64,
}

impl AiLogger {
    /// Open a new log file at `path`. Parent directory created on demand.
    /// Replaces any previously open writer (closes it implicitly on drop).
    pub fn open(&mut self, path: PathBuf) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let file = File::create(&path)?;
        self.writer = Some(BufWriter::new(file));
        self.plan_counter = 0;
        Ok(())
    }

    /// Close the current writer, if any. Safe to call when already closed.
    pub fn close(&mut self) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.writer.is_some()
    }

    pub fn next_plan_id(&mut self) -> u64 {
        let id = self.plan_counter;
        self.plan_counter = self.plan_counter.saturating_add(1);
        id
    }

    /// Write one entry as a single JSON line, flushed immediately so crashes
    /// don't lose the last decision.
    pub fn write_entry<T: Serialize>(&mut self, entry: &T) -> std::io::Result<()> {
        let Some(w) = self.writer.as_mut() else { return Ok(()) };
        let json = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(w, "{json}")?;
        w.flush()
    }
}

// ── Entry schema ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AiLogEntry<'a> {
    pub schema_version: u32,
    pub plan_id: u64,
    pub timestamp_ms: u128,
    pub decision_time_ms: u64,
    pub round: u32,
    pub actor_id: u64,
    pub actor_name: &'a str,
    pub actor_pos: [i32; 2],
    pub actor_ap: i32,
    pub actor_max_ap: i32,
    pub actor_mp: i32,
    pub actor_max_mp: i32,
    pub plans_evaluated: usize,
    pub plans_shown: usize,
    pub snapshot: &'a BattleSnapshot,
    pub intent: IntentBlock<'a>,
    pub plans: Vec<PlanLogEntry<'a>>,
    pub committed_decision: DecisionBlock,
    /// True when the killable gate (step-3) fired on this entry. Stub
    /// `false` until step-3 ships; then populated by `apply_killable_gate`
    /// caller via `build_entry`.
    pub gate_applied: bool,
    /// Number of plans the gate masked to `-inf`. Stub `0` until step-3 ships.
    pub gate_pruned_count: usize,
    /// True if the actor is in a survival regime this decision — intent
    /// is `ProtectSelf` or selection_kind indicates panic fallback. Derived
    /// in `build_entry` from the intent block; stable across log versions.
    pub survival_mode_active: bool,
    /// True if any plan in the pool has `evaluation_mode == LastStand`.
    /// Derived in `build_entry` from the plan_entries slice.
    pub last_stand_active: bool,
    /// Frozen difficulty profile used by this decision. Makes the log
    /// self-contained for replay: re-running with the same profile
    /// reproduces the same scores without relying on external defaults.
    pub difficulty: DifficultyProfileSnapshot,
    /// Persistent memory state of this actor immediately before pick_action.
    /// `None` when AiMemory is at default (no prior decisions this combat).
    pub ai_memory: Option<AiMemorySnapshot>,
    /// Team-wide reservation state immediately before pick_action (before
    /// this actor's own reservations are written for the round).
    pub reservations: ReservationsSnapshot,
}

#[derive(Serialize)]
pub struct IntentBlock<'a> {
    pub intent: &'a TacticalIntent,
    pub selection_kind: &'static str,
    pub reason_text: &'a str,
    /// Structured reason payload. `kind` field matches `selection_kind`; the
    /// remaining fields carry the numeric context (hp_pct, danger, eff_hp, …)
    /// that was previously only available as formatted text in `reason_text`.
    pub reason: &'a IntentReason,
    /// P7: adaptation reason that switched the chosen plan's evaluation regime
    /// to `LastStand`, or `None` when the chosen plan was scored under
    /// `EvaluationMode::Default`. Previously embedded inside
    /// `IntentReason::Adapted`; now a parallel top-level field so `reason`
    /// carries the unmodified select_intent result and adaptation context lives
    /// here. Schema-additive: v33 logs without this field decode as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_mode_reason: Option<&'a AdaptationReason>,
}

#[derive(Serialize)]
pub struct PlanLogEntry<'a> {
    pub rank: usize,
    pub chosen: bool,
    pub steps: &'a [PlanStep],
    pub outcomes: &'a [StepOutcome],
    pub final_pos: [i32; 2],
    pub residual_ap: i32,
    pub residual_mp: i32,
    /// Per-plan factor decomposition (v29 named map, replaces legacy `raw_factors` array).
    /// Layout: `{damage, kill_now, kill_promised, cc, heal, scarcity, saturation,
    /// intent, tempo_gain, self_survival}`. Column order updated from v28.
    /// Offline tools can recalibrate weights by re-normalizing + re-scoring
    /// without rerunning sim. When `evaluation_mode = LastStand`, the `intent`
    /// slot reflects the `LastStand` rescore.
    pub factors: &'a crate::combat::ai::scoring::factors::PlanFactorValues,
    /// Terminal-state factor decomposition (v29 named map).
    pub terminal_factors: &'a crate::combat::ai::scoring::factors::FactorTerminalScore,
    /// Score after ADAPTATION (and noise). Kept under the historical name
    /// `score` so v1-v5 readers stay meaningful on v6 files. For adapted
    /// plans this is the LastStand-weighted number; for non-adapted plans
    /// it equals `base_score`.
    pub score: f32,
    /// Score **before** ADAPTATION (immediately after sanity, before the
    /// adaptation rescore). Equals `score` when `evaluation_mode =
    /// Default`. Useful for diagnosing "did adaptation matter here?"
    /// without rerunning the pipeline.
    pub base_score: f32,
    /// Which evaluation regime scored this plan's intent-column. See
    /// `adapt::EvaluationMode`.
    pub evaluation_mode: &'a EvaluationMode,
    /// Fact that triggered the mode switch for this plan, or `None` when
    /// `evaluation_mode == Default`. Parallel to `IntentReason::Adapted`,
    /// but per-plan rather than per-decision (the decision-wide reason is
    /// in `intent.reason`; this field exposes it for every plan in the
    /// pool, not just the chosen one).
    pub adaptation_reason: Option<&'a AdaptationReason>,
    /// MVP2 trade economics breakdown. `score` inside this block is the
    /// plan-level modifier actually added to the top-level `score`
    /// field, so a reader can subtract it and recover the pre-trade
    /// composition if desired. The HP-scaled decomposition
    /// (`delta`, `killed`, `lost`, `self_lost`, `self_lethal`) explains
    /// how the modifier arose.
    pub trade: TradeBlock,
    /// Per-rule sanity breakdown for this plan (step 0.3C). Each entry
    /// records one rule that fired and the multiplier it applied.
    /// Empty when no sanity rules fired for this plan.
    /// v15 logs without this field deserialize via `#[serde(default)]`
    /// to an empty slice.
    pub sanity_breakdown: Vec<SanityHit>,
    /// Per-step outcome annotations (step 4.5, schema v19). Each entry contains
    /// an `ActionOutcomeEstimate` for the corresponding plan step. v18 logs
    /// deserialize via `#[serde(default)]` → empty annotation.
    pub annotation: &'a PlanAnnotation,
}

/// Serialised form of `combat::ai::trade::TradeBreakdown` plus the
/// post-tanh score contribution. Fields are HP-equivalent except
/// `score` (already normalised + weighted) and `self_lethal` (flag).
#[derive(Serialize, Default)]
pub struct TradeBlock {
    /// `killed - lost - self_lost`; the input to `tanh`.
    pub delta: f32,
    /// Σ `unit_value` over enemies the plan kills within its commit
    /// prefix.
    pub killed: f32,
    /// Σ `unit_value` over allies the plan kills within its commit
    /// prefix (including the actor via self-AoE friendly fire).
    pub lost: f32,
    /// `unit_value(self)` if the plan is self-lethal via AoO and the
    /// actor is not already in a sim kill list. `0.0` otherwise.
    pub self_lost: f32,
    /// True when the plan is expected to terminate the actor this
    /// turn — via sim kill-list membership or EV-lethal AoO.
    pub self_lethal: bool,
    /// `trade::trade_score(delta, actor_value)` — the post-tanh
    /// weighted number added to the composite score. Matches the
    /// increment the scorer applied verbatim.
    pub score: f32,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
pub enum DecisionBlock {
    EndTurn,
    CastInPlace { ability: String, target_id: u64, target_pos: [i32; 2] },
    MoveAndCast {
        path: Vec<[i32; 2]>,
        ability: String,
        target_id: u64,
        target_pos: [i32; 2],
    },
    MoveOnlyRetreat { path: Vec<[i32; 2]> },
    MoveCloser { path: Vec<[i32; 2]> },
}

impl From<&AiDecision> for DecisionBlock {
    fn from(d: &AiDecision) -> Self {
        fn path(v: &[Hex]) -> Vec<[i32; 2]> {
            v.iter().map(|h| [h.x, h.y]).collect()
        }
        match d {
            AiDecision::EndTurn => Self::EndTurn,
            AiDecision::CastInPlace { ability, target, target_pos } => Self::CastInPlace {
                ability: ability.0.clone(),
                target_id: target.to_bits(),
                target_pos: [target_pos.x, target_pos.y],
            },
            AiDecision::MoveAndCast { path: p, ability, target, target_pos } => Self::MoveAndCast {
                path: path(p),
                ability: ability.0.clone(),
                target_id: target.to_bits(),
                target_pos: [target_pos.x, target_pos.y],
            },
            AiDecision::Move { path: p, origin } => match origin {
                crate::combat::ai::orchestration::MoveOrigin::BestPlan => {
                    Self::MoveOnlyRetreat { path: path(p) }
                }
                crate::combat::ai::orchestration::MoveOrigin::Fallback => {
                    Self::MoveCloser { path: path(p) }
                }
            },
        }
    }
}

// ── Filename construction ──────────────────────────────────────────────────

/// Compact UTC timestamp `YYYYMMDDTHHMMSS` derived from epoch seconds.
/// Pure algorithm — avoids pulling `chrono` for a single formatter. Uses
/// Howard Hinnant's civil calendar conversion.
pub fn format_timestamp_utc(epoch_s: u64) -> String {
    let day_seconds = epoch_s % 86_400;
    let h = day_seconds / 3600;
    let m = (day_seconds % 3600) / 60;
    let s = day_seconds % 60;
    let days_since_epoch = (epoch_s / 86_400) as i64;

    // civil_from_days: days since 1970-01-01 → (year, month, day).
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}{month:02}{d:02}T{h:02}{m:02}{s:02}")
}

/// Replace non-alphanumeric-and-hyphen chars with `_` so the segment is safe
/// to embed in a filename across platforms.
pub fn sanitize_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Build the per-combat log path under `logs/` relative to the CWD.
pub fn build_combat_log_path(
    campaign: &str,
    scenario: &str,
    encounter: &str,
    now_epoch_s: u64,
) -> PathBuf {
    let ts = format_timestamp_utc(now_epoch_s);
    let name = format!(
        "{ts}_{}_{}_{}.jsonl",
        sanitize_for_filename(campaign),
        sanitize_for_filename(scenario),
        sanitize_for_filename(encounter),
    );
    PathBuf::from("logs").join(name)
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Millis since Unix epoch, or 0 if the clock is before epoch (shouldn't
/// happen; keeps the signature infallible for log-site ergonomics).
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Build a `PlanLogEntry` from a plan + its raw factors, per-adaptation
/// score pair, trade breakdown, evaluation-mode metadata, and sanity
/// breakdown. `chosen` reflects whether this plan was the one `pick_action`
/// committed.
pub fn plan_to_log_entry<'a>(
    plan: &'a TurnPlan,
    rank: usize,
    chosen: bool,
    factors: &'a crate::combat::ai::scoring::factors::PlanFactorValues,
    terminal_factors: &'a crate::combat::ai::scoring::factors::FactorTerminalScore,
    base_score: f32,
    score: f32,
    evaluation_mode: &'a EvaluationMode,
    adaptation_reason: Option<&'a AdaptationReason>,
    trade: TradeBlock,
    sanity_breakdown: Vec<SanityHit>,
) -> PlanLogEntry<'a> {
    PlanLogEntry {
        rank,
        chosen,
        steps: &plan.steps,
        outcomes: &plan.outcomes,
        final_pos: [plan.final_pos.x, plan.final_pos.y],
        residual_ap: plan.residual_ap,
        residual_mp: plan.residual_mp,
        factors,
        terminal_factors,
        score,
        base_score,
        evaluation_mode,
        adaptation_reason,
        trade,
        sanity_breakdown,
        annotation: &plan.annotation,
    }
}

/// System: open a fresh log file for the combat we're entering. Runs on
/// `OnEnter(AppState::Combat)`. Silently no-op if `ai_log` setting is off or
/// the required scenario state isn't available. Failure to create the file is
/// a `warn!` — the game proceeds without logging.
pub fn open_ai_log_on_combat_enter(
    settings: Res<crate::content::settings::GameSettings>,
    scenario: Option<Res<crate::game::resources::ScenarioState>>,
    campaign: Option<Res<crate::game::resources::CampaignState>>,
    db: Res<crate::game::resources::GameDb>,
    mut logger: ResMut<AiLogger>,
) {
    if !settings.ai_log {
        return;
    }
    let Some(scen_state) = scenario else { return };
    let Some(scen_def) = db.scenarios.get(&scen_state.scenario_id) else { return };
    let encounter_id = match scen_def.scenes.get(scen_state.scene_index) {
        Some(crate::content::scenarios::SceneDef::Combat { encounter_id, .. }) => {
            encounter_id.as_str()
        }
        _ => "unknown",
    };
    let campaign_id = campaign
        .as_ref()
        .map(|c| c.campaign_id.as_str())
        .unwrap_or("standalone");

    let now_epoch_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let path = build_combat_log_path(campaign_id, &scen_state.scenario_id, encounter_id, now_epoch_s);
    match logger.open(path.clone()) {
        Ok(()) => info!("AI log → {}", path.display()),
        Err(e) => warn!("AI log open failed at {}: {}", path.display(), e),
    }
}

/// System: close the current log writer on `OnExit(AppState::Combat)` so
/// each combat produces a self-contained file.
pub fn close_ai_log_on_combat_exit(mut logger: ResMut<AiLogger>) {
    logger.close();
}

/// Build an entry for a given decision. Caller fills the `plans` list and
/// provides the snapshot + intent + actor data via its owning scope.
pub fn build_entry<'a>(
    plan_id: u64,
    decision_time_ms: u64,
    active: &'a UnitSnapshot,
    actor_name: &'a str,
    snapshot: &'a BattleSnapshot,
    intent: IntentBlock<'a>,
    plans_evaluated: usize,
    plans_shown: usize,
    plan_entries: Vec<PlanLogEntry<'a>>,
    decision: &AiDecision,
    gate_applied: bool,
    gate_pruned_count: usize,
    difficulty: &DifficultyProfile,
    memory: &AiMemory,
    reservations_snap: ReservationsSnapshot,
) -> AiLogEntry<'a> {
    // P7: LastStand is no longer a TacticalIntent variant; survival_mode_active
    // covers ProtectSelf intents and adapt-triggered LastStand (via evaluation_mode_reason).
    let survival_mode_active = matches!(intent.intent, TacticalIntent::ProtectSelf)
        || intent.selection_kind.starts_with("protect_self")
        || intent.evaluation_mode_reason.is_some();

    let last_stand_active = plan_entries
        .iter()
        .any(|p| matches!(p.evaluation_mode, EvaluationMode::LastStand));

    AiLogEntry {
        schema_version: SCHEMA_VERSION,
        plan_id,
        timestamp_ms: now_ms(),
        decision_time_ms,
        round: snapshot.round,
        actor_id: active.entity.to_bits(),
        actor_name,
        actor_pos: [active.pos.x, active.pos.y],
        actor_ap: active.action_points,
        actor_max_ap: active.max_ap,
        actor_mp: active.movement_points,
        actor_max_mp: active.speed,
        plans_evaluated,
        plans_shown,
        snapshot,
        intent,
        plans: plan_entries,
        committed_decision: DecisionBlock::from(decision),
        gate_applied,
        gate_pruned_count,
        survival_mode_active,
        last_stand_active,
        difficulty: DifficultyProfileSnapshot::from(difficulty),
        ai_memory: AiMemorySnapshot::from_memory(memory),
        reservations: reservations_snap,
    }
}

// (Plan divergence log types removed in v27 clean-break — divergence data
// now lives inside `ActorTickEvent.continuation` per tick.)

// ── Replay snapshot wire types (v17+) ─────────────────────────────────────

/// Frozen `DifficultyProfile` captured at decision time for self-contained replay.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DifficultyProfileSnapshot {
    pub awareness: f32,
    pub decision_quality: f32,
    pub intent_commitment: f32,
    pub survival_instinct: f32,
    pub resource_discipline: f32,
    pub coordination: f32,
    pub mercy: f32,
    pub plan_max_depth: usize,
    pub plan_beam_width: usize,
    pub plan_step_discount: f32,
    pub damage_horizon_rounds: u32,
}

impl From<&DifficultyProfile> for DifficultyProfileSnapshot {
    fn from(d: &DifficultyProfile) -> Self {
        Self {
            awareness: d.awareness,
            decision_quality: d.decision_quality,
            intent_commitment: d.intent_commitment,
            survival_instinct: d.survival_instinct,
            resource_discipline: d.resource_discipline,
            coordination: d.coordination,
            mercy: d.mercy,
            plan_max_depth: d.plan_max_depth,
            plan_beam_width: d.plan_beam_width,
            plan_step_discount: d.plan_step_discount,
            damage_horizon_rounds: d.damage_horizon_rounds,
        }
    }
}

impl From<&DifficultyProfileSnapshot> for DifficultyProfile {
    fn from(s: &DifficultyProfileSnapshot) -> Self {
        Self {
            awareness: s.awareness,
            decision_quality: s.decision_quality,
            intent_commitment: s.intent_commitment,
            survival_instinct: s.survival_instinct,
            resource_discipline: s.resource_discipline,
            coordination: s.coordination,
            mercy: s.mercy,
            plan_max_depth: s.plan_max_depth,
            plan_beam_width: s.plan_beam_width,
            plan_step_discount: s.plan_step_discount,
            damage_horizon_rounds: s.damage_horizon_rounds,
        }
    }
}

/// Wire format for `StoredGoalContext` in JSONL logs (step 6.6, schema v25).
///
/// Replaces the removed `StoredPlanSnapshot` in `AiMemorySnapshot`. Flattens
/// `Hex` to `[q, r]` arrays and `Entity` to u64 bits so no non-serializable
/// types leak into the log schema.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredGoalContextSnapshot {
    /// Short code from `GoalKind::code()` (e.g. `"finish"`, `"pressure"`).
    pub kind: String,
    /// Entity bits of the target, if applicable.
    pub target_id: Option<u64>,
    /// Region anchor as `[q, r]`.
    pub region_anchor: [i32; 2],
    pub region_radius: u32,
    /// Ability id string of the planned ability, if any.
    pub planned_ability: Option<String>,
    pub ttl: u8,
    pub confidence: f32,
    pub created_round: u32,
    // Severity-check fields (for replay parity — step 6.6).
    pub expected_actor_pos: [i32; 2],
    pub actor_hp_at_store: i32,
    pub actor_rage_at_store: i32,
    pub actor_status_hash: u64,
    /// Status ids stored with the goal — used to compute delta for
    /// `actor_status_changed` severity classification (step 9.B.3).
    /// `#[serde(default)]` for backward compat with pre-v30 snapshots.
    #[serde(default)]
    pub actor_statuses_at_store: Vec<String>,
    pub target_hp_at_store: i32,
    pub target_pos_at_store: [i32; 2],
}

impl From<&StoredGoalContext> for StoredGoalContextSnapshot {
    fn from(g: &StoredGoalContext) -> Self {
        Self {
            kind: g.kind.code().to_owned(),
            target_id: g.kind.target_entity().map(|e| e.to_bits()),
            region_anchor: [g.region_anchor.x, g.region_anchor.y],
            region_radius: g.region_radius,
            planned_ability: g.planned_ability.as_ref().map(|a| a.0.clone()),
            ttl: g.ttl,
            confidence: g.confidence,
            created_round: g.created_round,
            expected_actor_pos: [g.expected_actor_pos.x, g.expected_actor_pos.y],
            actor_hp_at_store: g.actor_hp_at_store,
            actor_rage_at_store: g.actor_rage_at_store,
            actor_status_hash: g.actor_status_hash,
            actor_statuses_at_store: g.actor_statuses_at_store.iter().map(|s| s.0.clone()).collect(),
            target_hp_at_store: g.target_hp_at_store,
            target_pos_at_store: [g.target_pos_at_store.x, g.target_pos_at_store.y],
        }
    }
}

impl From<&StoredGoalContextSnapshot> for StoredGoalContext {
    /// Reconstruct a `StoredGoalContext` from a log snapshot.
    ///
    /// Used by offline tools (miner, replay) to call `classify_continuation_outcome`
    /// on logged data. The `GoalKind` is reconstructed from the `kind` string and
    /// `target_id`; for kinds requiring an entity (`finish`, `pressure`, `disable_enemy`,
    /// `heal_ally`) a missing or invalid `target_id` falls back to `GoalKind::Reposition`.
    fn from(s: &StoredGoalContextSnapshot) -> Self {
        let anchor = Hex::new(s.region_anchor[0], s.region_anchor[1]);
        let target_entity = s.target_id.and_then(Entity::try_from_bits);

        let kind = match s.kind.as_str() {
            "finish" => target_entity
                .map(|t| GoalKind::Finish { target: t })
                .unwrap_or(GoalKind::Reposition { region_center: anchor }),
            "pressure" => target_entity
                .map(|t| GoalKind::Pressure { target: t })
                .unwrap_or(GoalKind::Reposition { region_center: anchor }),
            "disable_enemy" => target_entity
                .map(|t| GoalKind::DisableEnemy { target: t })
                .unwrap_or(GoalKind::Reposition { region_center: anchor }),
            "heal_ally" => target_entity
                .map(|t| GoalKind::HealAlly { ally: t })
                .unwrap_or(GoalKind::Reposition { region_center: anchor }),
            "retreat" => GoalKind::Retreat { region_anchor: anchor },
            "setup_aoe" => {
                if let Some(ability_str) = &s.planned_ability {
                    GoalKind::SetupAOE {
                        region_center: anchor,
                        planned_ability: AbilityId(ability_str.clone()),
                    }
                } else {
                    GoalKind::Reposition { region_center: anchor }
                }
            }
            _ => GoalKind::Reposition { region_center: anchor },
        };

        StoredGoalContext {
            kind,
            region_anchor: anchor,
            region_radius: s.region_radius,
            planned_ability: s
                .planned_ability
                .as_ref()
                .map(|a| AbilityId(a.clone())),
            ttl: s.ttl,
            confidence: s.confidence,
            created_round: s.created_round,
            expected_actor_pos: Hex::new(s.expected_actor_pos[0], s.expected_actor_pos[1]),
            actor_hp_at_store: s.actor_hp_at_store,
            actor_rage_at_store: s.actor_rage_at_store,
            actor_status_hash: s.actor_status_hash,
            actor_statuses_at_store: s
                .actor_statuses_at_store
                .iter()
                .map(|id| crate::core::StatusId::from(id.as_str()))
                .collect(),
            target_hp_at_store: s.target_hp_at_store,
            target_pos_at_store: Hex::new(s.target_pos_at_store[0], s.target_pos_at_store[1]),
        }
    }
}

/// Persistent actor memory captured immediately before pick_action.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AiMemorySnapshot {
    pub last_intent: Option<IntentKind>,
    /// `last_target` entity serialized as u64 bits; `None` when no target.
    pub last_target: Option<u64>,
    pub turns_committed: u8,
    /// v25+: stored goal context from the previous Move decision.
    /// Replaces `last_plan` (`StoredPlanSnapshot`) removed in step 6.6.
    /// `None` for fresh actors, after Cast/EndTurn, or in pre-v25 logs
    /// (backward-compat via `#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_goal: Option<StoredGoalContextSnapshot>,
    /// v24 legacy: `last_plan` kept as an ignored field so v24 logs
    /// deserialize without errors. Always `None` on v25+ logs.
    /// The field is not re-emitted (skip_serializing_if).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_plan: Option<serde_json::Value>,
    /// v20+: HP ratio of the actor at the previous decision time.
    /// `None` for fresh actors or pre-v20 logs.
    #[serde(default)]
    pub hp_ratio_at_last_turn: Option<f32>,
    /// v20+: Whether the previous intent was defensive (`ProtectSelf` / `LastStand`).
    /// Defaults to `false` for fresh actors or pre-v20 logs.
    #[serde(default)]
    pub last_turn_was_defensive: bool,
    /// v20+: Consecutive turns the actor was in the low-HP zone before this decision.
    /// Defaults to `0` for fresh actors or pre-v20 logs.
    #[serde(default)]
    pub turns_in_low_hp: u8,
}

impl AiMemorySnapshot {
    /// Build from live `AiMemory`. Returns `None` when memory is fully default
    /// (no prior decisions), matching the `Option<AiMemorySnapshot>` field
    /// semantics in `AiLogEntry` — keeps the JSON compact for fresh actors.
    pub fn from_memory(m: &AiMemory) -> Option<Self> {
        if m.last_intent.is_none() && m.last_target.is_none()
            && m.turns_committed == 0 && m.last_goal.is_none()
            && m.hp_ratio_at_last_turn.is_none()
            && !m.last_turn_was_defensive
            && m.turns_in_low_hp == 0
        {
            return None;
        }
        Some(Self {
            last_intent: m.last_intent,
            last_target: m.last_target.map(|e| e.to_bits()),
            turns_committed: m.turns_committed,
            last_goal: m.last_goal.as_ref().map(StoredGoalContextSnapshot::from),
            last_plan: None, // always None on v25+ — field kept for backward compat read of v24 logs
            hp_ratio_at_last_turn: m.hp_ratio_at_last_turn,
            last_turn_was_defensive: m.last_turn_was_defensive,
            turns_in_low_hp: m.turns_in_low_hp,
        })
    }
}

/// Team-wide reservation state captured immediately before pick_action.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ReservationsSnapshot {
    /// Damage already claimed on each target entity (entity bits → f32).
    pub damage: std::collections::HashMap<u64, f32>,
    /// Entities that have CC already reserved (as entity bits).
    pub cc: std::collections::HashSet<u64>,
    /// Tiles claimed by earlier actors this round (as `[x, y]`).
    pub tiles: std::collections::HashSet<[i32; 2]>,
}

// ── Schema v27: unified actor_tick event ──────────────────────────────────

/// Logged decision variant for `ActorTickEvent` (schema v27).
///
/// Mirrors `AiDecision` but uses plain serializable types (u64, [i32;2], Vec)
/// instead of Bevy `Entity` / `Hex` / `AbilityId`. Includes `Skip` for the
/// early-return path (no AP/MP) which `AiDecision` does not model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoggedDecision {
    Cast {
        ability: String,
        target: u64,
        target_pos: [i32; 2],
    },
    MoveAndCast {
        path: Vec<[i32; 2]>,
        ability: String,
        target: u64,
        target_pos: [i32; 2],
    },
    Move {
        path: Vec<[i32; 2]>,
    },
    EndTurn,
    Skip {
        reason: String,
    },
}

/// A single plan entry in `ActorTickEvent.plans` (schema v27).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoggedPlan {
    pub rank: usize,
    pub steps: Vec<PlanStep>,
    pub annotation: PlanAnnotation,
}

/// Continuation section of `ActorTickEvent` — present when a stored goal
/// existed at the start of the tick (before `goal_lifecycle::pre_tick`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContinuationLogSection {
    pub stored_goal: StoredGoalContextSnapshot,
    /// Severity of the mismatch between the stored goal and current state.
    /// `None` when the world still matches (no mismatch detected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<ContinuationSeverity>,
    /// Age of the stored goal in rounds: `current_round - stored.created_round`.
    pub age: u32,
}

/// Lightweight serialisation form of one agenda item (step 11.6, schema v32).
///
/// Carries the intent kind, optional target, heuristic score, item-level
/// baseline considerations, and the reason that produced this item.  The
/// full `AgendaItem` is not serialised because it is too large and the
/// per-plan `PerItemEval` overlay (with plan-aware corrections) is already
/// captured in `PlanAnnotation.considerations_per_item`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgendaItemLog {
    /// Intent kind (without entity payload — entity is in `target`).
    pub kind: IntentKind,
    /// Target entity bits; `None` for non-targeted intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u64>,
    /// Heuristic raw score from the band-specific builder.
    pub raw_score: f32,
    /// Item-level baseline considerations (plan-agnostic, from `build_agenda`).
    pub considerations: IntentConsiderations,
    /// Diagnostic reason that produced this agenda item.
    pub reason: IntentReason,
}

/// Unified per-tick AI decision event (schema v27).
///
/// Replaces the old `actor_turn` + `plan_divergence` + implicit skip-path.
/// Self-contained: each record carries the full snapshot so tools can work
/// on individual entries without cross-record correlation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActorTickEvent {
    pub event_type: String,
    pub schema_version: u32,
    pub round: u32,
    pub timestamp_ms: u64,
    pub actor_id: u64,
    pub actor_name: String,
    pub snapshot: BattleSnapshot,
    /// Sorted by rank (1 = best). Empty on skip-path.
    pub plans: Vec<LoggedPlan>,
    pub decision: LoggedDecision,
    /// Present when a stored goal existed at tick start (before pre_tick ran).
    /// `None` on first tick of an actor's turn or after a Cast/Move&Cast cleared
    /// the stored goal.
    pub continuation: Option<ContinuationLogSection>,
    /// `IntentReason` of the chosen plan — full structured reason (panic_override,
    /// taunt_forced, killable, etc.). `None` on skip-path. Required by mining
    /// to distinguish reactive vs voluntary abandons via `IntentReason::code()`.
    pub intent_reason: Option<crate::combat::ai::intent::IntentReason>,
    /// P7 (schema v34): adaptation reason that switched the chosen plan's evaluation
    /// regime to `LastStand`, or `None` when no adaptation fired for the chosen plan.
    /// Parallel to `intent_reason` — carries only the adaptation context that was
    /// previously embedded in `IntentReason::Adapted`. Schema-additive: v33 without
    /// this field decodes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_mode_reason: Option<crate::combat::ai::adapt::AdaptationReason>,
    /// Step 11.6 (schema v32): priority band assigned to the actor this tick.
    /// `None` on skip-path (no AP/MP — band was not computed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<PriorityBand>,
    /// Step 11.6 (schema v32): structured reason for band assignment.
    /// `None` on skip-path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_reason: Option<BandReason>,
    /// Step 11.6 (schema v32): agenda items built for this tick.
    /// Empty on skip-path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agenda: Vec<AgendaItemLog>,
}

// ── ActorTickInput + build helpers ────────────────────────────────────────

/// All inputs needed to assemble an `ActorTickEvent`.
pub struct ActorTickInput<'a> {
    pub round: u32,
    pub actor: Entity,
    pub actor_name: &'a str,
    pub snapshot: &'a BattleSnapshot,
    /// Stored goal captured **before** `goal_lifecycle::pre_tick` ran.
    pub memory_pre: &'a Option<StoredGoalContext>,
    /// The committed AI decision for this tick.
    pub decision: &'a AiDecision,
    /// Non-`None` for the early-return (no AP/MP) path; `None` for the full path.
    pub skip_reason: Option<&'static str>,
    /// Scored pool from `pick_action`; `None` on skip-path.
    pub pool: Option<&'a crate::combat::ai::pipeline::ScoredPool>,
    /// `IntentReason` of the chosen plan from `pick_action`; `None` on skip-path.
    /// Threaded into `ActorTickEvent.intent_reason` for mining tools.
    pub intent_reason: Option<&'a crate::combat::ai::intent::IntentReason>,
    /// P7: adaptation reason from `PickResult.evaluation_mode_reason`; `None` on
    /// skip-path or when no adaptation fired for the chosen plan.
    pub evaluation_mode_reason: Option<&'a crate::combat::ai::adapt::AdaptationReason>,
    pub debug_names: &'a std::collections::HashMap<Entity, String>,
    /// Status tag cache for severity classification in `continuation` section.
    pub status_tags: &'a crate::combat::ai::world::tags::StatusTagCache,
    /// Step 11.6: priority band assigned this tick. `None` on skip-path.
    pub band: Option<(PriorityBand, BandReason)>,
    /// Step 11.6: agenda built this tick. `None` on skip-path.
    pub agenda: Option<&'a crate::combat::ai::intent::agenda::Agenda>,
}

/// Build an `ActorTickEvent` from the given inputs. Pure function.
///
/// - On skip-path (`skip_reason.is_some()`): `plans = []`, `decision = Skip`.
/// - On full path: plans from pool sorted by score desc, decision from `AiDecision`.
/// - `continuation` populated when `memory_pre` is `Some`.
pub fn build_actor_tick_event(input: ActorTickInput<'_>) -> ActorTickEvent {
    let actor_id = input.actor.to_bits();

    // Build LoggedDecision.
    let decision = if let Some(reason) = input.skip_reason {
        LoggedDecision::Skip { reason: reason.to_owned() }
    } else {
        logged_decision_from_ai(input.decision)
    };

    // Build plans list (empty on skip-path).
    let plans = if input.skip_reason.is_some() {
        vec![]
    } else if let Some(pool) = input.pool {
        build_logged_plans(pool)
    } else {
        vec![]
    };

    // Build continuation section.
    let continuation = input.memory_pre.as_ref().map(|stored| {
        let actor_snap = input.snapshot.unit(input.actor);
        let target_snap = stored.target_entity().and_then(|t| input.snapshot.unit(t));
        let severity = stored.check_continuation(
            actor_snap.unwrap_or_else(|| input.snapshot.units.first().expect("non-empty snap")),
            target_snap,
            input.status_tags,
        ).map(|c| c.severity);
        let age = input.round.saturating_sub(stored.created_round);
        ContinuationLogSection {
            stored_goal: StoredGoalContextSnapshot::from(stored),
            severity,
            age,
        }
    });

    // Build agenda log (empty on skip-path).
    let (band, band_reason, agenda) = if let Some((b, br)) = input.band {
        let agenda_log = input.agenda.map(|ag| {
            ag.items.iter().map(|item| AgendaItemLog {
                kind: item.kind,
                target: item.target.map(|e| e.to_bits()),
                raw_score: item.raw_score,
                considerations: item.considerations,
                reason: item.reason.clone(),
            }).collect()
        }).unwrap_or_default();
        (Some(b), Some(br), agenda_log)
    } else {
        (None, None, vec![])
    };

    ActorTickEvent {
        event_type: "actor_tick".to_owned(),
        schema_version: SCHEMA_VERSION,
        round: input.round,
        timestamp_ms: now_ms() as u64,
        actor_id,
        actor_name: input.actor_name.to_owned(),
        snapshot: input.snapshot.clone(),
        plans,
        decision,
        intent_reason: input.intent_reason.cloned(),
        evaluation_mode_reason: input.evaluation_mode_reason.cloned(),
        continuation,
        band,
        band_reason,
        agenda,
    }
}

/// Convert an `AiDecision` to a `LoggedDecision`. Panics if called with a
/// decision that has no direct mapping (there are none currently).
fn logged_decision_from_ai(decision: &AiDecision) -> LoggedDecision {
    match decision {
        AiDecision::CastInPlace { ability, target, target_pos } => LoggedDecision::Cast {
            ability: ability.0.clone(),
            target: target.to_bits(),
            target_pos: [target_pos.x, target_pos.y],
        },
        AiDecision::MoveAndCast { path, ability, target, target_pos } => {
            LoggedDecision::MoveAndCast {
                path: path.iter().map(|h| [h.x, h.y]).collect(),
                ability: ability.0.clone(),
                target: target.to_bits(),
                target_pos: [target_pos.x, target_pos.y],
            }
        }
        AiDecision::Move { path, .. } => LoggedDecision::Move {
            path: path.iter().map(|h| [h.x, h.y]).collect(),
        },
        AiDecision::EndTurn => LoggedDecision::EndTurn,
    }
}

/// Sort pool plans by score descending and convert to `LoggedPlan` list.
fn build_logged_plans(pool: &crate::combat::ai::pipeline::ScoredPool) -> Vec<LoggedPlan> {
    use crate::combat::ai::pipeline::score_trace::ScoreTraceLog;

    let mut indexed: Vec<(usize, f32)> = pool
        .annotations
        .iter()
        .enumerate()
        .map(|(i, a)| (i, a.score))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed
        .into_iter()
        .enumerate()
        .map(|(rank_idx, (pool_idx, _score))| {
            // pool.annotations[pool_idx] holds all pipeline-stage data (score,
            // sanity, adaptation, chosen, …) but was default-constructed in
            // ScoredPool::new and therefore has outcomes = [].
            // pool.plans[pool_idx].annotation.outcomes was populated during
            // plan generation (generator.rs) and is the authoritative source.
            // Merge: start from the pipeline annotation, then fill outcomes
            // from the generator-side annotation so both halves are present.
            let mut annotation = pool.annotations[pool_idx].clone();
            annotation.outcomes = pool.plans[pool_idx].annotation.outcomes.clone();
            // P3b: populate the serialisation mirror from the runtime trace.
            annotation.score_trace_log = Some(ScoreTraceLog::from(&annotation.score_trace));
            LoggedPlan {
                rank: rank_idx + 1,
                steps: pool.plans[pool_idx].steps.clone(),
                annotation,
            }
        })
        .collect()
}

/// Serialize `ActorTickEvent` as a JSONL line and write to logger.
pub fn write_actor_tick_log(logger: &mut AiLogger, input: ActorTickInput<'_>) {
    if !logger.is_enabled() {
        return;
    }
    let event = build_actor_tick_event(input);
    if let Err(e) = logger.write_entry(&event) {
        warn!("AI actor_tick log write failed: {}", e);
    }
}

// ── Schema-versioned parsing ───────────────────────────────────────────────

/// Error returned by [`parse_actor_tick`].
#[derive(Debug)]
pub enum LogError {
    /// The log entry's `schema_version` is lower than `SCHEMA_VERSION`.
    /// Clean break — use a newer playtest to generate v28+ logs.
    UnsupportedSchema {
        found: u32,
        required: u32,
        hint: &'static str,
    },
    /// JSON parse error.
    Json(serde_json::Error),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::UnsupportedSchema { found, required, hint } => {
                write!(f, "unsupported schema v{found} (required v{required}): {hint}")
            }
            LogError::Json(e) => write!(f, "JSON parse error: {e}"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<serde_json::Error> for LogError {
    fn from(e: serde_json::Error) -> Self {
        LogError::Json(e)
    }
}

/// Minimal struct for reading `schema_version` before full parse.
#[derive(Deserialize)]
struct SchemaHeader {
    #[serde(default)]
    schema_version: u32,
}

/// Parse a single JSONL line as an [`ActorTickEvent`], rejecting old schemas
/// with a clear error.
///
/// v33 logs are schema-additive with v34: `evaluation_mode_reason` was added
/// in v34 (P7) but all other fields are identical, so v33 logs parse
/// successfully with `evaluation_mode_reason` defaulting to `None`.
///
/// Returns `Err(LogError::UnsupportedSchema)` when the line carries a
/// `schema_version` lower than 33. Tools should show this error to the user
/// and ask for a fresh v33+ playtest.
pub fn parse_actor_tick(line: &str) -> Result<ActorTickEvent, LogError> {
    let header: SchemaHeader = serde_json::from_str(line)?;
    // v33 is schema-additive with v34 (evaluation_mode_reason absent → None).
    // v32 and below are hard breaks (score_trace_log and earlier fields missing).
    const MIN_SUPPORTED: u32 = SCHEMA_VERSION - 1; // = 33
    if header.schema_version < MIN_SUPPORTED {
        return Err(LogError::UnsupportedSchema {
            found: header.schema_version,
            required: SCHEMA_VERSION,
            hint: "v32 logs pre-date score_trace_log (v33); rebuild logs from v33+ playtest",
        });
    }
    Ok(serde_json::from_str(line)?)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_block_round_trips_via_json() {
        let d = AiDecision::EndTurn;
        let b: DecisionBlock = (&d).into();
        let s = serde_json::to_string(&b).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["kind"], "EndTurn");
    }

    #[test]
    fn plan_id_monotonic() {
        let mut logger = AiLogger::default();
        assert_eq!(logger.next_plan_id(), 0);
        assert_eq!(logger.next_plan_id(), 1);
        assert_eq!(logger.next_plan_id(), 2);
    }

    #[test]
    fn timestamp_format_known_epoch() {
        // 2026-04-19 14:30:22 UTC = epoch 1_776_609_022 (verified via CPython
        // datetime.timestamp()).
        assert_eq!(format_timestamp_utc(1_776_609_022), "20260419T143022");
        // 1970-01-01 00:00:00 UTC → baseline.
        assert_eq!(format_timestamp_utc(0), "19700101T000000");
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize_for_filename("foo bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize_for_filename("safe-name_42"), "safe-name_42");
        assert_eq!(sanitize_for_filename("a:b?c*d"), "a_b_c_d");
    }

    #[test]
    fn log_path_has_expected_shape() {
        let p = build_combat_log_path("main", "scene1", "goblin_camp", 1_776_609_022);
        let s = p.to_string_lossy();
        assert!(s.starts_with("logs"), "prefix logs/: {s}");
        assert!(s.ends_with("20260419T143022_main_scene1_goblin_camp.jsonl"), "{s}");
    }

    #[test]
    fn entry_serializes_current_schema_telemetry_fields() {
        // Minimal AiLogEntry constructed directly to verify current schema fields
        // appear in the JSON output with the expected types. AiLogEntry has no
        // Deserialize derive (lifetime refs), so we assert via serde_json::Value.
        use crate::combat::ai::config::difficulty::DifficultyProfile;

        let snap = BattleSnapshot::default();
        let intent_val = TacticalIntent::ProtectSelf;
        let reason_val = IntentReason::NoRuleDefault;
        let difficulty = DifficultyProfile::normal();
        let memory = crate::combat::ai::intent::AiMemory::default();
        let entry = AiLogEntry {
            schema_version: SCHEMA_VERSION,
            plan_id: 0,
            timestamp_ms: 0,
            decision_time_ms: 0,
            round: 1,
            actor_id: 0,
            actor_name: "test",
            actor_pos: [0, 0],
            actor_ap: 2,
            actor_max_ap: 2,
            actor_mp: 3,
            actor_max_mp: 3,
            plans_evaluated: 0,
            plans_shown: 0,
            snapshot: &snap,
            intent: IntentBlock {
                intent: &intent_val,
                selection_kind: "protect_self",
                reason_text: "",
                reason: &reason_val,
                evaluation_mode_reason: None,
            },
            plans: vec![],
            committed_decision: DecisionBlock::EndTurn,
            gate_applied: true,
            gate_pruned_count: 3,
            survival_mode_active: true,
            last_stand_active: false,
            difficulty: DifficultyProfileSnapshot::from(&difficulty),
            ai_memory: AiMemorySnapshot::from_memory(&memory),
            reservations: ReservationsSnapshot::default(),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["gate_applied"], true);
        assert_eq!(v["gate_pruned_count"], 3);
        assert_eq!(v["survival_mode_active"], true);
        assert_eq!(v["last_stand_active"], false);
        // v17+ snapshot sections are present.
        assert!(v["difficulty"].is_object(), "difficulty section present");
        assert!(v["ai_memory"].is_null(), "fresh actor → null ai_memory");
        assert!(v["reservations"].is_object(), "reservations section present");
    }

    #[test]
    fn difficulty_snapshot_round_trips() {
        use crate::combat::ai::config::difficulty::DifficultyProfile;
        let snap = DifficultyProfileSnapshot::from(&DifficultyProfile::hard());
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: DifficultyProfileSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.awareness, snap.awareness);
        assert_eq!(back.plan_max_depth, snap.plan_max_depth);
        assert_eq!(back.damage_horizon_rounds, snap.damage_horizon_rounds);
    }

    #[test]
    fn ai_memory_snapshot_round_trips() {
        use crate::combat::ai::intent::AiMemory;
        // Non-default memory to exercise the Some path.
        let m = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            turns_committed: 2,
            ..AiMemory::default()
        };
        let snap = AiMemorySnapshot::from_memory(&m).expect("non-default → Some");
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: AiMemorySnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.last_intent, Some(IntentKind::FocusTarget));
        assert_eq!(back.turns_committed, 2);
        assert!(back.last_target.is_none());
        assert!(back.last_plan.is_none());
    }

    #[test]
    fn reservations_snapshot_round_trips() {
        use crate::combat::ai::world::reservations::Reservations;
        use crate::game::hex::Hex;
        let e = Entity::from_raw_u32(42).expect("valid");
        let mut r = Reservations::default();
        r.reserve_damage(e, 15.5);
        r.reserve_cc(e);
        r.reserve_tile(Hex::new(3, -1));
        let snap = r.to_snapshot();
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: ReservationsSnapshot = serde_json::from_str(&json).expect("deserialize");
        let restored = Reservations::from_snapshot(&back);
        assert_eq!(restored.reserved_damage(e), 15.5);
        assert!(restored.has_reserved_cc(e));
        assert!(restored.is_tile_reserved(Hex::new(3, -1)));
    }

    #[test]
    fn open_replaces_previous_writer() {
        // Temp file via std — avoids pulling tempfile crate.
        let mut logger = AiLogger::default();
        assert!(!logger.is_enabled());
        let dir = std::env::temp_dir().join("storyforge_log_tests");
        let p1 = dir.join("a.jsonl");
        let p2 = dir.join("b.jsonl");
        logger.open(p1.clone()).expect("open1");
        assert!(logger.is_enabled());
        logger.open(p2.clone()).expect("open2");
        assert!(logger.is_enabled(), "still open with new file");
        logger.close();
        assert!(!logger.is_enabled());
        // Cleanup.
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
    }

    // ── actor_tick event (schema v27) ─────────────────────────────────────────

    fn make_tick_input_skip<'a>(
        actor: Entity,
        snap: &'a BattleSnapshot,
        debug_names: &'a std::collections::HashMap<Entity, String>,
    ) -> ActorTickInput<'a> {
        ActorTickInput {
            round: 1,
            actor,
            actor_name: "TestEnemy",
            snapshot: snap,
            memory_pre: &None,
            decision: &AiDecision::EndTurn,
            skip_reason: Some("no_ap_no_mp"),
            pool: None,
            intent_reason: None,
            evaluation_mode_reason: None,
            debug_names,
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            band: None,
            agenda: None,
        }
    }

    #[test]
    fn actor_tick_event_round_trips() {
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(1);
        let input = make_tick_input_skip(actor, &snap, &debug_names);
        let event = build_actor_tick_event(input);
        let json = serde_json::to_string(&event).expect("serialize");
        let restored: ActorTickEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event.event_type, restored.event_type);
        assert_eq!(event.schema_version, restored.schema_version);
        assert_eq!(event.round, restored.round);
        assert_eq!(event.actor_id, restored.actor_id);
        assert_eq!(event.actor_name, restored.actor_name);
        assert_eq!(event.plans.len(), restored.plans.len());
        assert_eq!(event.decision, restored.decision);
        assert_eq!(event.continuation.is_none(), restored.continuation.is_none());
    }

    #[test]
    fn build_actor_tick_event_skip_uses_skip_decision_kind() {
        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(2);
        let input = make_tick_input_skip(actor, &snap, &debug_names);
        let event = build_actor_tick_event(input);
        assert!(
            matches!(event.decision, LoggedDecision::Skip { .. }),
            "expected Skip, got {:?}", event.decision
        );
        assert_eq!(event.plans.len(), 0, "skip path must have empty plans");
        assert_eq!(event.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn build_actor_tick_event_full_has_chosen_annotation() {
        use crate::combat::ai::pipeline::ScoredPool;
        use crate::combat::ai::plan::types::TurnPlan;

        // Build a minimal pool with two plans; second plan is chosen.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let mut pool = ScoredPool::new(plans);
        pool.annotations[0].score = 1.0;
        pool.annotations[1].score = 2.0;
        pool.annotations[1].chosen = true;

        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(3);
        let decision = AiDecision::EndTurn;
        let test_reason = crate::combat::ai::intent::IntentReason::NoRuleDefault;
        let input = ActorTickInput {
            round: 2,
            actor,
            actor_name: "Boss",
            snapshot: &snap,
            memory_pre: &None,
            decision: &decision,
            skip_reason: None,
            pool: Some(&pool),
            intent_reason: Some(&test_reason),
            evaluation_mode_reason: None,
            debug_names: &debug_names,
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            band: None,
            agenda: None,
        };
        let event = build_actor_tick_event(input);

        // Plans sorted by score desc: rank 1 = score 2.0 (chosen), rank 2 = score 1.0.
        assert_eq!(event.plans.len(), 2, "two plans in pool");
        let chosen_count = event.plans.iter().filter(|p| p.annotation.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan has chosen=true");
        assert_eq!(event.plans[0].rank, 1, "rank 1 is highest score");
        assert!(event.plans[0].annotation.chosen, "rank 1 plan is the chosen one");
    }

    /// Regression guard: annotation.outcomes populated in the generator must survive
    /// through build_logged_plans into the serialized ActorTickEvent.
    ///
    /// Root cause of the v28 playtest bug: ScoredPool::new zero-fills pool.annotations
    /// (no outcomes), while outcomes live in pool.plans[i].annotation.outcomes.
    /// build_logged_plans was cloning only pool.annotations — silently dropping outcomes.
    #[test]
    fn build_logged_plans_preserves_annotation_outcomes() {
        use crate::combat::ai::outcome::ActionOutcomeEstimate;
        use crate::combat::ai::pipeline::ScoredPool;
        use crate::combat::ai::plan::types::TurnPlan;

        // Build a plan whose generator-side annotation has one outcome entry.
        let mut plan = TurnPlan::default();
        plan.annotation.outcomes.push(ActionOutcomeEstimate::default());

        let mut pool = ScoredPool::new(vec![plan]);
        pool.annotations[0].score = 1.0;
        pool.annotations[0].chosen = true;

        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(7);
        let decision = AiDecision::EndTurn;
        let reason = crate::combat::ai::intent::IntentReason::NoRuleDefault;
        let input = ActorTickInput {
            round: 1,
            actor,
            actor_name: "Tester",
            snapshot: &snap,
            memory_pre: &None,
            decision: &decision,
            skip_reason: None,
            pool: Some(&pool),
            intent_reason: Some(&reason),
            evaluation_mode_reason: None,
            debug_names: &debug_names,
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            band: None,
            agenda: None,
        };
        let event = build_actor_tick_event(input);

        assert_eq!(event.plans.len(), 1);
        assert_eq!(
            event.plans[0].annotation.outcomes.len(), 1,
            "annotation.outcomes must be preserved through build_logged_plans"
        );
    }

    // ── parse_actor_tick schema version tests ─────────────────────────────────

    /// v27 log line returns `UnsupportedSchema` error — clean break.
    #[test]
    fn parse_v27_returns_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":27}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 27, required: 34, .. })),
            "v27 must produce UnsupportedSchema(found=27, required=34), got: {result:?}",
        );
    }

    /// v26 log line also returns `UnsupportedSchema` error.
    #[test]
    fn parse_v26_returns_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":26}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 26, required: 34, .. })),
            "v26 must produce UnsupportedSchema(found=26, required=34), got: {result:?}",
        );
    }

    /// v28 log line returns `UnsupportedSchema` — wire format break (raw_factors → factors named map).
    #[test]
    fn actor_tick_v28_load_yields_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":28}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 28, required: 34, .. })),
            "v28 must produce UnsupportedSchema(found=28, required=34), got: {result:?}",
        );
    }

    /// v29 log line returns `UnsupportedSchema` — step 9.B bump (actor_statuses fields added in v30).
    #[test]
    fn actor_tick_v29_load_yields_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":29}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 29, required: 34, .. })),
            "v29 must produce UnsupportedSchema(found=29, required=34), got: {result:?}",
        );
    }

    /// v30 log line returns `UnsupportedSchema` — step 10.4 bump (critics serialised, SanityRule shrunk).
    #[test]
    fn actor_tick_v30_load_yields_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":30}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 30, required: 34, .. })),
            "v30 must produce UnsupportedSchema(found=30, required=34), got: {result:?}",
        );
    }

    /// v31 log line returns `UnsupportedSchema` — step 11.6 bump (bands/agenda serialisation in v32).
    #[test]
    fn actor_tick_v31_load_yields_unsupported_schema_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":31}"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 31, required: 34, .. })),
            "v31 must produce UnsupportedSchema(found=31, required=34), got: {result:?}",
        );
    }

    /// Minimal current-schema actor_tick round-trips through parse_actor_tick.
    #[test]
    fn actor_tick_current_schema_round_trip() {
        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(1);
        let input = make_tick_input_skip(actor, &snap, &debug_names);
        let event = build_actor_tick_event(input);
        assert_eq!(event.schema_version, SCHEMA_VERSION);

        let json = serde_json::to_string(&event).expect("serialize");
        let parsed = parse_actor_tick(&json).expect("parse_actor_tick should succeed for current schema");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.actor_id, event.actor_id);
    }

    /// v29/v32/v33 logs contain legacy fields (`modifiers`, `sanity`, `critics`, `contract`)
    /// that are absent from TLE-3a structs. Serde ignore-unknown ensures backward compat.
    #[test]
    fn annotation_with_legacy_fields_deserialises_via_ignore_unknown() {
        // JSON that includes fields removed in TLE-3a. Serde must silently ignore them.
        let json = r#"{"score": 1.5, "modifiers": [], "sanity": [], "critics": [], "contract": null}"#;
        let ann: crate::combat::ai::outcome::PlanAnnotation =
            serde_json::from_str(json).expect("PlanAnnotation with legacy fields must parse");
        assert!(
            (ann.score - 1.5_f32).abs() < 1e-6,
            "score must be preserved: {}",
            ann.score
        );
    }

    /// v32 round-trip: tick with band/agenda/considerations_per_item serialises
    /// and deserialises correctly. Validates schema v32 additions and that v32
    /// is accepted by the v33 parser (schema-additive backward compat).
    #[test]
    fn schema_v32_round_trip() {
        use crate::combat::ai::intent::bands::{BandReason, PriorityBand};
        use crate::combat::ai::intent::considerations::IntentConsiderations;
        use crate::combat::ai::intent::{IntentKind, IntentReason};
        use crate::combat::ai::pipeline::ScoredPool;
        use crate::combat::ai::plan::types::TurnPlan;

        let snap = BattleSnapshot::default();
        let debug_names = std::collections::HashMap::new();
        let actor = Entity::from_bits(42);
        let decision = AiDecision::EndTurn;
        let reason = IntentReason::NoRuleDefault;

        let mut plan = TurnPlan::default();
        let cons = IntentConsiderations {
            urgency: 0.8, feasibility: 0.9, leverage: 0.7,
            safety: 0.6, role_affinity: 0.5, continuation_value: 0.4,
        };
        plan.annotation.considerations_per_item = vec![cons];
        plan.annotation.agenda_item = Some(0);

        let mut pool = ScoredPool::new(vec![plan]);
        pool.annotations[0].score = 3.15;
        pool.annotations[0].chosen = true;
        pool.annotations[0].considerations_per_item = vec![cons];
        pool.annotations[0].agenda_item = Some(0);

        let band = PriorityBand::NormalTactical;
        let band_reason = BandReason::Normal;

        use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
        let agenda_item = AgendaItem {
            kind: IntentKind::FocusTarget,
            target: Some(Entity::from_bits(99)),
            raw_score: 1.5,
            reason: IntentReason::NoRuleDefault,
            considerations: cons,
        };
        let agenda = Agenda { band, items: vec![agenda_item] };

        let input = ActorTickInput {
            round: 1,
            actor,
            actor_name: "TestActor",
            snapshot: &snap,
            memory_pre: &None,
            decision: &decision,
            skip_reason: None,
            pool: Some(&pool),
            intent_reason: Some(&reason),
            evaluation_mode_reason: None,
            debug_names: &debug_names,
            status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
            band: Some((band, band_reason)),
            agenda: Some(&agenda),
        };

        let event = build_actor_tick_event(input);
        // schema_version is now 33 (P3b bump).
        assert_eq!(event.schema_version, SCHEMA_VERSION, "must be current schema version");
        assert!(event.band.is_some(), "band must be present on full path");
        assert_eq!(event.band, Some(PriorityBand::NormalTactical));
        assert_eq!(event.agenda.len(), 1, "agenda must have one item");
        assert_eq!(event.agenda[0].kind, IntentKind::FocusTarget);
        assert_eq!(event.agenda[0].target, Some(99u64));
        assert!((event.agenda[0].raw_score - 1.5).abs() < 1e-6);

        // Check considerations_per_item and score_trace_log in the logged plan.
        assert_eq!(event.plans.len(), 1);
        let logged_ann = &event.plans[0].annotation;
        assert_eq!(logged_ann.agenda_item, Some(0u8));
        assert_eq!(logged_ann.considerations_per_item.len(), 1);
        assert!((logged_ann.considerations_per_item[0].urgency - 0.8).abs() < 1e-6);
        // P3b: score_trace_log must be populated by build_logged_plans.
        assert!(logged_ann.score_trace_log.is_some(), "score_trace_log must be present after P3b");

        // Full JSON round-trip at current schema.
        let json = serde_json::to_string(&event).expect("serialize");
        let restored: ActorTickEvent = parse_actor_tick(&json).expect("parse_actor_tick current schema");
        assert_eq!(restored.schema_version, SCHEMA_VERSION);
        assert_eq!(restored.band, Some(PriorityBand::NormalTactical));
        assert_eq!(restored.agenda.len(), 1);
        assert_eq!(restored.plans[0].annotation.agenda_item, Some(0u8));
        assert_eq!(restored.plans[0].annotation.considerations_per_item.len(), 1);
        // score_trace_log survives the round-trip.
        assert!(restored.plans[0].annotation.score_trace_log.is_some());
    }

    /// v32 corpus is now rejected (MIN_SUPPORTED = 33). v32 pre-dates
    /// score_trace_log (added in v33) — hard break, not schema-additive.
    #[test]
    fn schema_v32_rejected_as_pre_score_trace_log() {
        let json = r#"{
            "event_type": "actor_tick",
            "schema_version": 32,
            "round": 1,
            "timestamp_ms": 0,
            "actor_id": 1,
            "actor_name": "x",
            "snapshot": {"units": [], "round": 0},
            "plans": [],
            "decision": {"kind": "end_turn"},
            "continuation": null,
            "band": null,
            "band_reason": null,
            "agenda": []
        }"#;
        let result = parse_actor_tick(json);
        assert!(
            matches!(result, Err(LogError::UnsupportedSchema { found: 32, required: 34, .. })),
            "v32 must produce UnsupportedSchema(found=32, required=34), got: {result:?}",
        );
    }

    /// v33 corpus (without evaluation_mode_reason field) is accepted by the v34 parser.
    /// evaluation_mode_reason defaults to None — schema-additive backward compat.
    #[test]
    fn schema_v33_accepted_as_additive_with_evaluation_mode_reason_none() {
        let json = r#"{
            "event_type": "actor_tick",
            "schema_version": 33,
            "round": 1,
            "timestamp_ms": 0,
            "actor_id": 1,
            "actor_name": "x",
            "snapshot": {"units": [], "round": 0},
            "plans": [{"rank": 1, "steps": [], "annotation": {"score": 2.5}}],
            "decision": {"kind": "end_turn"},
            "continuation": null,
            "band": null,
            "band_reason": null,
            "agenda": []
        }"#;
        let event = parse_actor_tick(json).expect("v33 must parse as schema-additive");
        assert_eq!(event.schema_version, 33);
        assert_eq!(event.plans.len(), 1);
        assert!(
            event.evaluation_mode_reason.is_none(),
            "evaluation_mode_reason absent in v33 → None"
        );
    }

    /// v31 format must yield UnsupportedSchema with a hint mentioning v32 score_trace_log.
    #[test]
    fn schema_v31_rejected_with_clear_error() {
        let json = r#"{"event_type":"actor_tick","schema_version":31,"round":1,"timestamp_ms":0,"actor_id":1,"actor_name":"x","plans":[],"decision":{"kind":"end_turn"},"snapshot":{}}"#;
        let result = parse_actor_tick(json);
        let Err(LogError::UnsupportedSchema { found, required, hint: _ }) = result else {
            panic!("expected UnsupportedSchema, got: {result:?}");
        };
        assert_eq!(found, 31);
        assert_eq!(required, 34);
    }
}
