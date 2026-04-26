//! Offline replay pipeline — shared between the `replay_ai_log` binary and
//! integration tests (`tests/ai_scenarios.rs`, `tests/replay_assert.rs`).
//!
//! Two layers:
//!
//! - **Serde-mirror types** ([`LogEntry`], [`PlanLog`], [`IntentBlock`],
//!   [`LoggedTradeBlock`], [`LoggedEvaluationMode`], [`LoggedAdaptationReason`])
//!   own a deserializable copy of each JSONL line. Shapes mirror
//!   `combat::ai::log` with `#[serde(default)]` on every field added after
//!   `SCHEMA_VERSION = 1`, so older logs still parse.
//!
//! - **Assertion pipeline** ([`assert_log_file`]) re-runs the production
//!   scoring pipeline (`finalize_scores` → `sanity_adjust_plans` →
//!   optional `apply_protect_self_mask` → `pick_best_plan`) on a captured
//!   JSONL entry, reconstructs the chosen decision via
//!   [`replay_assertion::build_actual_decision`], and compares it against
//!   an overlay loaded from `*.expected.toml`. Returns an [`AssertOutcome`]
//!   with both the raw decision and the pass/fail verdict.
//!
//! Tests call [`assert_log_file`] directly (no subprocess); the
//! `replay_ai_log` binary wraps it with CLI-level I/O and exit codes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use bevy::prelude::Entity;
use serde::{Deserialize, Serialize};

use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::factors::{NUM_FACTORS, PlanFactors};
use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::log::{
    AiMemorySnapshot, DifficultyProfileSnapshot, ReservationsSnapshot,
};
use crate::combat::ai::planning::{
    apply_protect_self_mask, finalize_scores, pick_best_plan, sanity_adjust_plans, EvaluationMode,
    PlanStep, StepOutcome, TurnPlan,
};
use crate::combat::ai::replay_assertion::{
    build_actual_decision, run_assertion, ActualDecision, AssertResult, Overlay,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::{AiWorld, ScoringCtx};
use crate::combat::ai::SanityHit;
use crate::content::content_view::ContentView;
use crate::core::DiceRng;
use crate::game::hex::Hex;

// ── Serde mirror of log::AiLogEntry ──────────────────────────────────────────

/// Deserializable mirror of `combat::ai::log::AiLogEntry`. Owns all nested
/// state. Newly added fields use `#[serde(default)]` so older logs still
/// parse — see `log.rs::SCHEMA_VERSION` history.
#[derive(Deserialize)]
pub struct LogEntry {
    pub schema_version: u32,
    pub plan_id: u64,
    /// Deserialized from JSONL for schema completeness; not consumed by replay logic.
    #[allow(dead_code)]
    timestamp_ms: u128,
    pub decision_time_ms: u64,
    pub round: u32,
    pub actor_id: u64,
    pub actor_name: String,
    pub actor_ap: i32,
    pub actor_max_ap: i32,
    pub actor_mp: i32,
    /// Deserialized from JSONL for schema completeness; not consumed by replay logic.
    #[allow(dead_code)]
    actor_max_mp: i32,
    pub plans_evaluated: usize,
    /// Deserialized from JSONL for schema completeness; not consumed by replay logic.
    #[allow(dead_code)]
    plans_shown: usize,
    pub snapshot: BattleSnapshot,
    pub intent: IntentBlock,
    pub plans: Vec<PlanLog>,
    pub committed_decision: serde_json::Value,
    /// v15+: killable gate telemetry. v14 and earlier default to false/0.
    #[serde(default)]
    pub gate_applied: bool,
    #[serde(default)]
    pub gate_pruned_count: usize,
    #[serde(default)]
    pub survival_mode_active: bool,
    #[serde(default)]
    pub last_stand_active: bool,
    /// v17+: difficulty profile frozen at decision time. v16 and earlier
    /// logs return `None` — the caller falls back to `DifficultyProfile::normal()`.
    #[serde(default)]
    pub difficulty: Option<DifficultyProfileSnapshot>,
    /// v17+: actor memory state before `pick_action`. `None` for fresh
    /// actors or v16 logs.
    #[serde(default)]
    pub ai_memory: Option<AiMemorySnapshot>,
    /// v17+: team reservation state before `pick_action`. v16 and earlier
    /// default to empty.
    #[serde(default)]
    pub reservations: Option<ReservationsSnapshot>,
}

#[derive(Deserialize)]
pub struct IntentBlock {
    pub intent: TacticalIntent,
    pub selection_kind: String,
    #[serde(default, rename = "reason_text")]
    pub _reason_text: String,
}

#[derive(Deserialize)]
pub struct PlanLog {
    pub rank: usize,
    pub chosen: bool,
    pub steps: Vec<PlanStep>,
    pub outcomes: Vec<StepOutcome>,
    pub final_pos: [i32; 2],
    pub residual_ap: i32,
    pub residual_mp: i32,
    /// Length varies with schema version; callers pad/truncate to
    /// [`NUM_FACTORS`]. See `log.rs` history for layout changes.
    pub raw_factors: Vec<f32>,
    /// `None` when the game pruned the plan before scoring (beam-search
    /// rejection). Replay treats absent scores as `NEG_INFINITY` so `argmax`
    /// skips them naturally.
    pub score: Option<f32>,
    #[serde(default)]
    pub base_score: Option<f32>,
    #[serde(default)]
    pub evaluation_mode: LoggedEvaluationMode,
    #[serde(default)]
    pub adaptation_reason: Option<LoggedAdaptationReason>,
    #[serde(default)]
    pub trade: LoggedTradeBlock,
    /// v16+: per-rule sanity breakdown. v15 and earlier default to empty.
    #[serde(default)]
    pub sanity_breakdown: Vec<SanityHit>,
    /// v23+: per-plan annotation including terminal-state evaluation.
    /// v22 and earlier logs default to empty annotation (zero-filled
    /// `TerminalScore`). Not consumed by scoring reconstruction — diagnostic
    /// only.
    #[serde(default)]
    pub annotation: LoggedPlanAnnotation,
}

/// Deserializable mirror of `combat::ai::outcome::PlanAnnotation`.
/// Only terminal-state data is exposed here; `outcomes` vec is not mirrored
/// (already covered by `PlanLog.outcomes`).
#[derive(Deserialize, Default)]
pub struct LoggedPlanAnnotation {
    /// v23+: terminal-state evaluation for this plan.
    /// Zero-filled for v22 and earlier logs.
    #[serde(default)]
    pub terminal: LoggedTerminalScore,
}

/// Deserializable mirror of `planning::terminal::TerminalScore`.
#[derive(Deserialize, Default, Clone, Copy, Debug)]
pub struct LoggedTerminalScore {
    #[serde(default)]
    pub exposure_at_end: f32,
    #[serde(default)]
    pub next_turn_lethality: f32,
    #[serde(default)]
    pub secure_kill: f32,
    #[serde(default)]
    pub ally_rescue: f32,
    #[serde(default)]
    pub board_control_gain: f32,
    #[serde(default)]
    pub line_actionability: f32,
    #[serde(default)]
    pub density_value: f32,
    #[serde(default)]
    pub pressure_spacing_zone: f32,
}

/// Mirrors `log::TradeBlock`. Verbose-only rendering; not consumed by the
/// scoring reconstruction.
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct LoggedTradeBlock {
    #[serde(default)]
    pub delta: f32,
    #[serde(default)]
    pub killed: f32,
    #[serde(default)]
    pub lost: f32,
    #[serde(default)]
    pub self_lost: f32,
    #[serde(default)]
    pub self_lethal: bool,
    #[serde(default)]
    pub score: f32,
}

/// Mirrors `planning::adaptation::EvaluationMode` for deserialization.
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoggedEvaluationMode {
    #[default]
    Default,
    LastStand,
}

impl LoggedEvaluationMode {
    pub fn is_adapted(self) -> bool {
        !matches!(self, LoggedEvaluationMode::Default)
    }
}

/// Mirrors `planning::adaptation::AdaptationReason`. Variants are unit
/// (numeric payloads are replay-diagnostic only, not consumed by scoring).
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoggedAdaptationReason {
    ExpectedSelfLethal {
        #[serde(default)]
        aoo_dmg: f32,
        #[serde(default)]
        actor_hp: i32,
    },
    ProtectSelfNoDefensive,
    ProtectSelfFutile {
        #[serde(default)]
        pending_dot: i32,
        #[serde(default)]
        actor_hp: i32,
    },
}

impl LoggedAdaptationReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
            Self::ProtectSelfFutile { .. } => "protect_self_futile",
        }
    }
}

/// Map a `TacticalIntent` variant to its string label used in overlays.
pub fn intent_kind(i: &TacticalIntent) -> &'static str {
    use TacticalIntent::*;
    match i {
        FocusTarget { .. } => "FocusTarget",
        ApplyCC { .. } => "ApplyCC",
        Reposition => "Reposition",
        ProtectSelf => "ProtectSelf",
        ProtectAlly { .. } => "ProtectAlly",
        SetupAOE => "SetupAOE",
        LastStand => "LastStand",
    }
}

// ── Assert pipeline ──────────────────────────────────────────────────────────

/// Outcome of running [`assert_log_file`] — contains both the reconstructed
/// decision and the pass/fail verdict. Non-`Fail` does not imply the test
/// logically passed; inspect [`AssertOutcome::result`].
#[derive(Debug)]
pub struct AssertOutcome {
    pub jsonl_path: PathBuf,
    pub overlay_path: PathBuf,
    /// `plan_id` of the log entry the overlay was resolved against (v26).
    /// For v27 entries this is 0 (v27 logs do not have a plan_id field).
    pub plan_id: u64,
    /// Index of the chosen plan within `entry.plans` after re-scoring.
    pub chosen_idx: usize,
    /// Schema version of the log entry (for callers that warn on pre-v17).
    pub schema_version: u32,
    /// Actor entity bits from the log entry (v27+). For v26 entries this is
    /// derived from `entry.actor_id`.
    pub actor_id: u64,
    pub actual: ActualDecision,
    pub result: AssertResult,
}

/// Distinguishes I/O / parse failures (fatal for the run) from assertion
/// verdicts (carried in [`AssertOutcome::result`]).
#[derive(Debug)]
pub enum AssertError {
    Io { path: PathBuf, source: std::io::Error },
    OverlayParse { path: PathBuf, source: toml::de::Error },
    EntryParse { path: PathBuf, source: serde_json::Error },
    NoMatchingEntry { path: PathBuf, plan_id: Option<u64> },
    InvalidActorId(u64),
    ActorNotFound { actor_id: u64 },
}

impl std::fmt::Display for AssertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io error on {}: {source}", path.display()),
            Self::OverlayParse { path, source } => {
                write!(f, "cannot parse overlay {}: {source}", path.display())
            }
            Self::EntryParse { path, source } => {
                write!(f, "cannot parse log entry in {}: {source}", path.display())
            }
            Self::NoMatchingEntry { path, plan_id } => match plan_id {
                Some(id) => write!(f, "no entry with plan_id={id} in {}", path.display()),
                None => write!(f, "no entries in {}", path.display()),
            },
            Self::InvalidActorId(id) => write!(f, "invalid actor entity bits: {id}"),
            Self::ActorNotFound { actor_id } => {
                write!(f, "actor {actor_id} not present in entry snapshot")
            }
        }
    }
}

impl std::error::Error for AssertError {}

/// Load an overlay from disk.
pub fn load_overlay(path: &Path) -> Result<Overlay, AssertError> {
    let src = std::fs::read_to_string(path).map_err(|source| AssertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&src).map_err(|source| AssertError::OverlayParse {
        path: path.to_path_buf(),
        source,
    })
}

/// Read JSONL from `path`, return the first entry whose `plan_id` matches
/// `target` (or the first non-divergence entry when `target` is `None`).
///
/// Lines with `event_type == "plan_divergence"` are skipped (different
/// schema). Parse errors on individual lines are propagated only when they
/// cause no entry to match; malformed intermediate lines are ignored to
/// mirror the binary's lenient behavior.
pub fn find_entry(path: &Path, target: Option<u64>) -> Result<LogEntry, AssertError> {
    let file = std::fs::File::open(path).map_err(|source| AssertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut last_parse_err: Option<serde_json::Error> = None;

    for line in reader.lines() {
        let line = line.map_err(|source| AssertError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if val.get("event_type").and_then(|v| v.as_str()) == Some("plan_divergence") {
                continue;
            }
        }
        let entry: LogEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                last_parse_err = Some(e);
                continue;
            }
        };
        match target {
            Some(id) if entry.plan_id == id => return Ok(entry),
            None => return Ok(entry),
            _ => {}
        }
    }

    if let Some(source) = last_parse_err {
        return Err(AssertError::EntryParse {
            path: path.to_path_buf(),
            source,
        });
    }
    Err(AssertError::NoMatchingEntry {
        path: path.to_path_buf(),
        plan_id: target,
    })
}

/// Run the production scoring pipeline on `entry` and return the
/// reconstructed chosen decision.
///
/// Mirrors the assert-mode branch of `replay_ai_log`: rebuild influence
/// maps → `finalize_scores` → `sanity_adjust_plans` →
/// `apply_protect_self_mask` (only when intent is `ProtectSelf`) →
/// `pick_best_plan` with a seeded RNG. The RNG seed is fixed (`0`) so the
/// result is deterministic across runs.
pub fn reconstruct_decision(
    entry: &LogEntry,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<(usize, ActualDecision), AssertError> {
    let actor = Entity::try_from_bits(entry.actor_id)
        .ok_or(AssertError::InvalidActorId(entry.actor_id))?;
    let active = entry
        .snapshot
        .unit(actor)
        .cloned()
        .ok_or(AssertError::ActorNotFound {
            actor_id: entry.actor_id,
        })?;

    // v16 and earlier logs lack frozen difficulty/reservations; fall back
    // to production defaults. Callers that care (CI) gate this on
    // schema_version upstream.
    let difficulty = entry
        .difficulty
        .as_ref()
        .map(DifficultyProfile::from)
        .unwrap_or_else(DifficultyProfile::normal);
    let reservations = entry
        .reservations
        .as_ref()
        .map(Reservations::from_snapshot)
        .unwrap_or_default();

    let maps = build_influence_maps(&entry.snapshot, actor, active.team, inf_cfg);
    let world = AiWorld {
        content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
    };
    let scoring_ctx = ScoringCtx {
        world: &world,
        maps: &maps,
        reservations: &reservations,
        snap: &entry.snapshot,
        active: &active,
        need_signals: Default::default(),
        last_goal: None, // replay: no stored goal context available from JSONL v23
    };

    let mut plans: Vec<TurnPlan> = entry
        .plans
        .iter()
        .map(|p| TurnPlan {
            steps: p.steps.clone(),
            final_pos: Hex::new(p.final_pos[0], p.final_pos[1]),
            residual_ap: p.residual_ap,
            residual_mp: p.residual_mp,
            outcomes: p.outcomes.clone(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        })
        .collect();
    let raw_factors: Vec<PlanFactors> = entry
        .plans
        .iter()
        .map(|p| {
            let mut arr = [0.0f32; NUM_FACTORS];
            for (i, &v) in p.raw_factors.iter().take(NUM_FACTORS).enumerate() {
                arr[i] = v;
            }
            PlanFactors::from_array(arr)
        })
        .collect();
    let modes: Vec<EvaluationMode> = entry
        .plans
        .iter()
        .map(|p| match p.evaluation_mode {
            LoggedEvaluationMode::Default => EvaluationMode::Default,
            LoggedEvaluationMode::LastStand => EvaluationMode::LastStand,
        })
        .collect();

    let mut scores = finalize_scores(&mut plans, &raw_factors, &scoring_ctx);
    let _ = sanity_adjust_plans(&mut scores, &plans, &scoring_ctx);
    if matches!(entry.intent.intent, TacticalIntent::ProtectSelf) {
        apply_protect_self_mask(&mut scores, &raw_factors, &modes, world.tuning.thresholds.self_survival_epsilon);
    }

    let mut rng = DiceRng::with_seed(0);
    let (chosen_idx, _) = pick_best_plan(&scores, &raw_factors, &world, &mut rng);

    let chosen_plan = &entry.plans[chosen_idx];
    let intent_kind_str = intent_kind(&entry.intent.intent);
    let actual = build_actual_decision(
        &chosen_plan.steps,
        chosen_plan.final_pos,
        intent_kind_str,
        content,
    );
    Ok((chosen_idx, actual))
}

/// Convenience: load overlay, locate the targeted log entry, reconstruct
/// the decision, run the overlay assertion, and return a combined outcome.
///
/// Does not print anything — callers decide how to format pass/fail.
pub fn assert_log_file(
    jsonl_path: &Path,
    overlay_path: &Path,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<AssertOutcome, AssertError> {
    let overlay = load_overlay(overlay_path)?;
    let target_plan_id: Option<u64> = overlay.scope.as_ref().and_then(|s| s.plan_id);
    let entry = find_entry(jsonl_path, target_plan_id)?;
    let (chosen_idx, actual) = reconstruct_decision(&entry, content, inf_cfg)?;
    let result = run_assertion(&actual, &overlay);
    Ok(AssertOutcome {
        jsonl_path: jsonl_path.to_path_buf(),
        overlay_path: overlay_path.to_path_buf(),
        plan_id: entry.plan_id,
        chosen_idx,
        schema_version: entry.schema_version,
        actor_id: entry.actor_id,
        actual,
        result,
    })
}

/// Derive the default overlay path for a JSONL file: append `.expected.toml`
/// to the full filename (matching the CLI convention documented in
/// `ai-replay.md`).
pub fn default_overlay_path(jsonl: &Path) -> PathBuf {
    let mut p = jsonl.to_path_buf();
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(".expected.toml");
    p.set_file_name(name);
    p
}

// ── Golden-replay ────────────────────────────────────────────────────────────

/// A single record in a golden baseline JSONL. Captures the decision that the
/// production scoring pipeline made for one log entry. Used by
/// `--capture-golden` / `--compare-golden` in `replay_ai_log`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoldenRecord {
    /// Source JSONL path (as provided on the CLI, for corpus matching).
    pub log_path: String,
    /// `LogEntry::plan_id` — uniquely identifies the decision within the log.
    pub plan_id: u64,
    /// `LogEntry::actor_id` entity bits.
    pub actor_id: u64,
    /// `"CastInPlace"`, `"MoveAndCast"`, `"Move"`, or `"EndTurn"`.
    pub decision_kind: String,
    /// Ability name; present only for Cast variants.
    pub cast_ability: Option<String>,
    /// Target entity bits; present only for Cast variants.
    pub cast_target: Option<u64>,
    /// Final hex position `[col, row]`.
    pub end_position: [i32; 2],
}

/// Read all valid decision entries from `path`, skipping `plan_divergence`
/// events and unparseable lines (with a warning).
///
/// Mirrors the lenient behavior of [`find_entry`] but collects all entries
/// instead of stopping at the first match.
pub fn read_entries(path: &Path) -> Result<Vec<LogEntry>, AssertError> {
    let file = std::fs::File::open(path).map_err(|source| AssertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|source| AssertError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if val.get("event_type").and_then(|v| v.as_str()) == Some("plan_divergence") {
                continue;
            }
        }
        match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                eprintln!("warning: skipping unparseable line in {}: {e}", path.display());
            }
        }
    }
    Ok(entries)
}

/// Build a [`GoldenRecord`] for `entry` by running the production scoring
/// pipeline via [`reconstruct_decision`].
///
/// `log_path` is stored verbatim (as provided on the CLI) for corpus matching.
pub fn golden_from_entry(
    entry: &LogEntry,
    log_path: &str,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<GoldenRecord, AssertError> {
    let (_chosen_idx, actual) = reconstruct_decision(entry, content, inf_cfg)?;
    Ok(GoldenRecord {
        log_path: log_path.to_owned(),
        plan_id: entry.plan_id,
        actor_id: entry.actor_id,
        decision_kind: actual.decision_kind,
        cast_ability: actual.cast_ability,
        cast_target: actual.cast_target,
        end_position: actual.end_position,
    })
}

// ── v27 assert pipeline ───────────────────────────────────────────────────────

/// v27 version of [`assert_log_file`].
///
/// Reads the first non-skip `ActorTickEvent` from `jsonl_path` (or the event
/// whose `actor_id` equals the `plan_id` in the overlay scope, when specified —
/// see note below), runs the production `pick_action` on its snapshot, and
/// compares the result against the overlay expectations.
///
/// **Note on `plan_id` in the overlay scope**: v27 logs do not have a `plan_id`
/// field. The overlay's `[scope].plan_id` is reinterpreted as the target `actor_id`
/// (entity bits) for v27 files. When absent the first non-skip event is used.
/// This matches how the existing `ai_scenarios` overlays use `plan_id` to select
/// a specific entry.
pub fn assert_v27_log_file(
    jsonl_path: &Path,
    overlay_path: &Path,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<AssertOutcome, AssertError> {
    use crate::combat::ai::intent::AiMemory;
    use crate::combat::ai::log::{ActorTickEvent, LoggedDecision};
    use crate::combat::ai::utility::pick_action;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::utility::AiWorld;
    use crate::combat::ai::difficulty::DifficultyProfile;

    let overlay = load_overlay(overlay_path)?;
    // In v27 we reinterpret plan_id as actor_id for entry selection.
    let target_actor_id: Option<u64> = overlay.scope.as_ref().and_then(|s| s.plan_id);

    let file = std::fs::File::open(jsonl_path).map_err(|source| AssertError::Io {
        path: jsonl_path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);

    let mut selected: Option<ActorTickEvent> = None;
    for line in reader.lines() {
        let line = line.map_err(|source| AssertError::Io {
            path: jsonl_path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() { continue; }

        // Schema version guard.
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
                continue;
            }
        }

        let event: ActorTickEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(source) => return Err(AssertError::EntryParse {
                path: jsonl_path.to_path_buf(),
                source,
            }),
        };

        // Skip events have no pick_action to assert against.
        if matches!(event.decision, LoggedDecision::Skip { .. }) {
            continue;
        }

        match target_actor_id {
            Some(id) if event.actor_id == id => { selected = Some(event); break; }
            None => { selected = Some(event); break; }
            _ => {}
        }
    }

    let event = selected.ok_or_else(|| AssertError::NoMatchingEntry {
        path: jsonl_path.to_path_buf(),
        plan_id: target_actor_id,
    })?;

    let actor = Entity::try_from_bits(event.actor_id)
        .ok_or(AssertError::InvalidActorId(event.actor_id))?;
    let active = event.snapshot.unit(actor).cloned().ok_or(AssertError::ActorNotFound {
        actor_id: event.actor_id,
    })?;

    let maps = build_influence_maps(&event.snapshot, actor, active.team, inf_cfg);
    let difficulty = DifficultyProfile::normal();
    let world = AiWorld {
        content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
    };
    let memory = AiMemory::default();
    let reservations = Reservations::default();
    let mut rng = DiceRng::with_seed(0);

    let result = pick_action(
        actor, active.pos, &world, &event.snapshot, &maps,
        &mut rng, &memory, &reservations, false, &Default::default(),
    );

    let chosen_idx = result.best_idx;
    let chosen_plan = result.pool.plans.get(chosen_idx);

    let intent_kind_str = {
        use crate::combat::ai::intent::TacticalIntent;
        match result.intent {
            TacticalIntent::FocusTarget { .. } => "FocusTarget",
            TacticalIntent::ApplyCC { .. } => "ApplyCC",
            TacticalIntent::Reposition => "Reposition",
            TacticalIntent::ProtectSelf => "ProtectSelf",
            TacticalIntent::ProtectAlly { .. } => "ProtectAlly",
            TacticalIntent::SetupAOE => "SetupAOE",
            TacticalIntent::LastStand => "LastStand",
        }
    };

    let actual = if let Some(plan) = chosen_plan {
        build_actual_decision(
            &plan.steps,
            [plan.final_pos.x, plan.final_pos.y],
            intent_kind_str,
            content,
        )
    } else {
        build_actual_decision(&[], [active.pos.x, active.pos.y], intent_kind_str, content)
    };

    let assert_result = run_assertion(&actual, &overlay);

    Ok(AssertOutcome {
        jsonl_path: jsonl_path.to_path_buf(),
        overlay_path: overlay_path.to_path_buf(),
        plan_id: 0, // v27 logs have no plan_id
        chosen_idx,
        schema_version: event.schema_version,
        actor_id: event.actor_id,
        actual,
        result: assert_result,
    })
}

#[cfg(test)]
mod golden_tests {
    use super::GoldenRecord;

    #[test]
    fn golden_record_json_roundtrip() {
        let rec = GoldenRecord {
            log_path: "logs/test.jsonl".to_owned(),
            plan_id: 42,
            actor_id: 99,
            decision_kind: "MoveAndCast".to_owned(),
            cast_ability: Some("melee_attack".to_owned()),
            cast_target: Some(12884901551),
            end_position: [3, 5],
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: GoldenRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(rec, back);
    }
}
