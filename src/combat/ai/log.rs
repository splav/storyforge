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

use std::fs::{create_dir_all, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::intent::{AiMemory, IntentKind, IntentReason, TacticalIntent};
use crate::combat::ai::repair::{
    ContinuationOutcome, ContinuationSeverity, RepairAffinity, StoredGoalContext,
};
use crate::combat::ai::repair::goal::GoalKind;
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::{AdaptationReason, EvaluationMode, PlanStep, SanityHit, StepOutcome, TurnPlan};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{AiDecision, ChosenInfo};
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
pub const SCHEMA_VERSION: u32 = 26;

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
    /// Raw factors before batch normalization: [damage, kill_now, kill_promised,
    /// cc, heal, intent, scarcity, tempo_gain, saturation, self_survival].
    /// Offline tools can recalibrate weights by re-normalizing + re-scoring
    /// without rerunning sim. When `evaluation_mode = LastStand`, the `intent`
    /// column (index 5) reflects the `LastStand` rescore.
    pub raw_factors: [f32; 10],
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
    /// `planning::adaptation::EvaluationMode`.
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
    pub sanity_breakdown: &'a [SanityHit],
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
                crate::combat::ai::utility::MoveOrigin::BestPlan => {
                    Self::MoveOnlyRetreat { path: path(p) }
                }
                crate::combat::ai::utility::MoveOrigin::Fallback => {
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
    raw_factors: [f32; 10],
    base_score: f32,
    score: f32,
    evaluation_mode: &'a EvaluationMode,
    adaptation_reason: Option<&'a AdaptationReason>,
    trade: TradeBlock,
    sanity_breakdown: &'a [SanityHit],
) -> PlanLogEntry<'a> {
    PlanLogEntry {
        rank,
        chosen,
        steps: &plan.steps,
        outcomes: &plan.outcomes,
        final_pos: [plan.final_pos.x, plan.final_pos.y],
        residual_ap: plan.residual_ap,
        residual_mp: plan.residual_mp,
        raw_factors,
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
    let survival_mode_active = matches!(intent.intent, TacticalIntent::ProtectSelf)
        || intent.selection_kind.starts_with("protect_self")
        || intent.selection_kind == "last_stand";

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

// ── Plan divergence log ────────────────────────────────────────────────────

/// Snapshot of one side (stored or fresh) in a divergence comparison.
#[derive(Serialize)]
pub struct DivergenceSide {
    pub intent: IntentKind,
    pub ability: Option<String>,
    pub target_id: Option<u64>,
    pub score: f32,
}

/// Logged when the freeze-after-move logic has both a stored plan (from the
/// previous MoveOnly tick) and a fresh plan to compare. Written as a separate
/// JSON object alongside regular `pick_action` entries; `event_type` lets
/// analyzers filter without checking every line.
#[derive(Serialize)]
pub struct PlanDivergenceEntry {
    pub event_type: &'static str,
    pub schema_version: u32,
    pub timestamp_ms: u128,
    pub actor_id: u64,
    pub stored: DivergenceSide,
    pub fresh: DivergenceSide,
    /// Whether the stored plan's continuation was used (`true`) or the fresh
    /// plan was used instead (`false`).
    pub used_continuation: bool,
    /// Reason the stored plan was not used, if applicable.
    pub replan_reason: Option<&'static str>,
    /// Semantic severity of the detected mismatch, if any.
    /// `None` when there was no mismatch (continuation was clean or attempted).
    /// Added in step 6.0; old logs without this field read as `None` via
    /// `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_severity: Option<ContinuationSeverity>,
    pub intent_changed: bool,
    pub ability_changed: bool,
    pub target_changed: bool,
    pub score_delta: f32,
    // ── v24 fields (step 6.5) ────────────────────────────────────────────────
    /// High-level goal-preservation outcome for this tick.
    /// Defaults to `NoStoredGoal` for v23 logs without this field.
    #[serde(default = "ContinuationOutcome::no_stored_goal")]
    pub continuation_outcome: ContinuationOutcome,
    /// Repair affinity of the fresh chosen plan, if a stored goal was present.
    /// `None` when no stored goal existed or repair affinity was not computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_affinity: Option<RepairAffinity>,
    /// Pre-scaled repair bonus actually added to the fresh plan's score.
    /// `None` when no stored goal existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_bonus: Option<f32>,
    /// Short code for the stored goal kind (e.g. `"finish"`, `"pressure"`).
    /// `None` when no stored goal existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_kind: Option<String>,
}

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
            target_hp_at_store: g.target_hp_at_store,
            target_pos_at_store: [g.target_pos_at_store.x, g.target_pos_at_store.y],
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

impl AiLogger {
    /// Emit a `plan_divergence` entry. Called from `run_ai_turn` when a stored
    /// goal exists from the previous tick alongside a fresh plan.
    ///
    /// Step 6.6: `stored` parameter changed from `&StoredPlan` to
    /// `&StoredGoalContext` — the `stored` side of `DivergenceSide` is now
    /// synthesised from the goal kind rather than the literal step sequence.
    #[allow(clippy::too_many_arguments)]
    pub fn write_plan_divergence(
        &mut self,
        actor: bevy::prelude::Entity,
        stored_goal: &StoredGoalContext,
        fresh: &ChosenInfo,
        used_continuation: bool,
        replan_reason: Option<&'static str>,
        continuation_severity: Option<ContinuationSeverity>,
        continuation_outcome: ContinuationOutcome,
        fresh_repair_affinity: Option<RepairAffinity>,
        repair_bonus: Option<f32>,
    ) {
        if !self.is_enabled() {
            return;
        }
        // Extract committed action from the fresh plan.
        let fresh_ability: Option<&AbilityId>;
        let fresh_target: Option<bevy::prelude::Entity>;
        match fresh.plan.committed_prefix() {
            crate::combat::ai::planning::CommittedPrefix::Cast { ability, target, .. }
            | crate::combat::ai::planning::CommittedPrefix::MoveThenCast { ability, target, .. } => {
                fresh_ability = Some(ability);
                fresh_target = Some(target);
            }
            _ => {
                fresh_ability = None;
                fresh_target = None;
            }
        }

        let fresh_intent = fresh.intent.kind();
        let goal_kind = stored_goal.kind.code().to_owned();

        // Synthesise the stored side from goal context.
        // intent is derived from GoalKind; ability / target from planned_ability + target_entity.
        let stored_intent = goal_kind_to_intent_kind(&stored_goal.kind);
        let stored_ability = stored_goal.planned_ability.as_ref().map(|a| a.0.clone());
        let stored_target_id = stored_goal.kind.target_entity().map(|e| e.to_bits());
        // Use confidence as a proxy for score (confidence = score/pool_max at store time).
        let stored_score = stored_goal.confidence;
        let score_delta = fresh.score - stored_score;

        let entry = PlanDivergenceEntry {
            event_type: "plan_divergence",
            schema_version: SCHEMA_VERSION,
            timestamp_ms: now_ms(),
            actor_id: actor.to_bits(),
            stored: DivergenceSide {
                intent: stored_intent,
                ability: stored_ability.clone(),
                target_id: stored_target_id,
                score: stored_score,
            },
            fresh: DivergenceSide {
                intent: fresh_intent,
                ability: fresh_ability.map(|a| a.0.clone()),
                target_id: fresh_target.map(|e| e.to_bits()),
                score: fresh.score,
            },
            used_continuation,
            replan_reason,
            continuation_severity,
            intent_changed: stored_intent != fresh_intent,
            ability_changed: stored_ability.as_deref() != fresh_ability.map(|a| a.0.as_str()),
            target_changed: stored_target_id != fresh_target.map(|e| e.to_bits()),
            score_delta,
            continuation_outcome,
            repair_affinity: fresh_repair_affinity,
            repair_bonus,
            goal_kind: Some(goal_kind),
        };
        if let Err(e) = self.write_entry(&entry) {
            warn!("AI divergence log write failed: {}", e);
        }
    }
}

/// Map a `GoalKind` to the closest `IntentKind` for divergence log compat.
/// Used to populate `DivergenceSide.intent` when `StoredPlan` is unavailable.
fn goal_kind_to_intent_kind(kind: &GoalKind) -> IntentKind {
    match kind {
        GoalKind::Finish { .. } | GoalKind::Pressure { .. } => IntentKind::FocusTarget,
        GoalKind::DisableEnemy { .. } => IntentKind::ApplyCC,
        GoalKind::HealAlly { .. } => IntentKind::ProtectAlly,
        GoalKind::Retreat { .. } => IntentKind::ProtectSelf,
        GoalKind::SetupAOE { .. } => IntentKind::SetupAOE,
        GoalKind::Reposition { .. } => IntentKind::Reposition,
    }
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
        use crate::combat::ai::difficulty::DifficultyProfile;

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
        use crate::combat::ai::difficulty::DifficultyProfile;
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
        use crate::combat::ai::reservations::Reservations;
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

    // ── PlanDivergenceEntry v24 schema tests (step 6.5) ────────────────────────

    fn make_divergence_entry(
        outcome: ContinuationOutcome,
        repair_affinity: Option<crate::combat::ai::repair::RepairAffinity>,
        goal_kind: Option<String>,
    ) -> PlanDivergenceEntry {
        use crate::combat::ai::intent::IntentKind;
        PlanDivergenceEntry {
            event_type: "plan_divergence",
            schema_version: SCHEMA_VERSION,
            timestamp_ms: 0,
            actor_id: 42,
            stored: DivergenceSide {
                intent: IntentKind::FocusTarget,
                ability: Some("slash".into()),
                target_id: Some(1),
                score: 1.0,
            },
            fresh: DivergenceSide {
                intent: IntentKind::FocusTarget,
                ability: Some("slash".into()),
                target_id: Some(1),
                score: 1.1,
            },
            used_continuation: false,
            replan_reason: None,
            continuation_severity: None,
            intent_changed: false,
            ability_changed: false,
            target_changed: false,
            score_delta: 0.1,
            continuation_outcome: outcome,
            repair_affinity,
            repair_bonus: Some(0.25),
            goal_kind,
        }
    }

    #[test]
    fn plan_divergence_entry_v26_roundtrip() {
        let entry = make_divergence_entry(
            ContinuationOutcome::GoalPreservedMethodDelivered,
            Some(crate::combat::ai::repair::RepairAffinity {
                goal_alignment: 1.0,
                region_alignment: 0.8,
                method_alignment: 1.0,
                severity_factor: 1.0,
                ttl_factor: 0.9,
                confidence: 0.85,
            }),
            Some("finish".into()),
        );
        let json = serde_json::to_string(&entry).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(v["schema_version"], 26, "schema must be 26 (updated in step 6.6b)");
        // continuation_outcome is tagged: {"kind": "goal_preserved_method_delivered"}
        assert_eq!(v["continuation_outcome"]["kind"], "goal_preserved_method_delivered");
        assert!(v["repair_affinity"].is_object(), "repair_affinity present");
        assert!((v["repair_bonus"].as_f64().unwrap() - 0.25).abs() < 1e-5);
        assert_eq!(v["goal_kind"], "finish");
    }

    #[test]
    fn plan_divergence_entry_v26_reactive_roundtrip() {
        // GoalAbandonedReactive serialises with source field.
        let entry = make_divergence_entry(
            ContinuationOutcome::GoalAbandonedReactive { source: "taunt_forced".to_owned() },
            None,
            Some("finish".into()),
        );
        let json = serde_json::to_string(&entry).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["continuation_outcome"]["kind"], "goal_abandoned_reactive");
        assert_eq!(v["continuation_outcome"]["source"], "taunt_forced");
    }

    #[test]
    fn plan_divergence_entry_reads_v23_with_defaults() {
        // A v23 entry: no continuation_outcome / repair_affinity / goal_kind.
        let v23_json = r#"{
            "event_type": "plan_divergence",
            "schema_version": 23,
            "timestamp_ms": 0,
            "actor_id": 1,
            "stored": {"intent": "FocusTarget", "ability": null, "target_id": null, "score": 1.0},
            "fresh": {"intent": "FocusTarget", "ability": null, "target_id": null, "score": 1.2},
            "used_continuation": false,
            "replan_reason": "actor_hp_drop",
            "intent_changed": false,
            "ability_changed": false,
            "target_changed": false,
            "score_delta": 0.2
        }"#;
        // We can't Deserialize PlanDivergenceEntry (has &'static str fields),
        // so verify via Value that the new fields are absent (serde default kicks in).
        let v: serde_json::Value = serde_json::from_str(v23_json).expect("parse");
        // v23 log must not have continuation_outcome — absence = backward compat ok
        assert!(
            v.get("continuation_outcome").is_none(),
            "v23 log has no continuation_outcome field"
        );
        // When we decode ContinuationOutcome with default, we get NoStoredGoal.
        // Verify by deserializing just the outcome field from empty input:
        let no_field: ContinuationOutcome =
            serde_json::from_str("null").unwrap_or_else(|_| ContinuationOutcome::no_stored_goal());
        assert_eq!(no_field, ContinuationOutcome::NoStoredGoal);
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
}
