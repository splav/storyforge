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
use serde::Serialize;

use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::{PlanStep, StepOutcome, TurnPlan};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::AiDecision;
use crate::game::hex::Hex;

/// Log schema version.
/// - v1 → v2: added `reactions_left` + `aoo_expected_damage` to `UnitSnapshot`.
///   v1 logs are still readable via `#[serde(default)]` on the new fields
///   (defaults: `reactions_left=1` — matches the only content-wide `Reactions::max`;
///   `aoo_expected_damage=None` — no damage info available).
pub const SCHEMA_VERSION: u32 = 2;

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
}

#[derive(Serialize)]
pub struct IntentBlock<'a> {
    pub intent: &'a TacticalIntent,
    pub selection_kind: &'static str,
    pub reason_text: &'a str,
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
    /// Raw factors before batch normalization: [damage, kill, cc, heal,
    /// position, risk, focus, intent, scarcity]. Offline tools can recalibrate
    /// weights by re-normalizing + re-scoring without rerunning sim.
    pub raw_factors: [f32; 9],
    /// Final composite score after normalization, role weights, difficulty
    /// multipliers and noise. Not reproducible offline (noise uses RNG); use
    /// `raw_factors` for deterministic replay.
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
            AiDecision::MoveOnlyRetreat { path: p } => Self::MoveOnlyRetreat { path: path(p) },
            AiDecision::MoveCloser { path: p } => Self::MoveCloser { path: path(p) },
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

/// Map an intent selection `reason` freetext prefix to a stable queryable
/// code. Prefixes are coupled to the literal strings produced inside
/// `select_intent` and the viability-guard in `utility::mod::pick_action`;
/// a prefix drift there requires updating this table.
///
/// Returns `"unclassified"` as a forward-compat fallback — analyzers can flag
/// these and prompt a classifier update.
pub fn classify_selection(reason: &str) -> &'static str {
    if reason.starts_with("panic:") {
        "panic_override"
    } else if reason.starts_with("hp%=") {
        // "hp%=X%<40% × danger=..." — non-panic urgency ProtectSelf branch.
        "urgency"
    } else if reason.starts_with("ally hp%=") {
        "protect_ally"
    } else if reason.starts_with("forced by taunt") {
        "taunt_forced"
    } else if reason.starts_with("CC the taunter") {
        "taunt_cc"
    } else if reason.starts_with("killable:") {
        "killable"
    } else if reason.starts_with("highest priority=") {
        "best_priority"
    } else if reason.starts_with("CC high-threat") || reason.starts_with("stun ") {
        "apply_cc"
    } else if reason.starts_with("AoE cluster") || reason.starts_with("cluster") {
        "setup_aoe"
    } else if reason.starts_with("reposition") || reason.starts_with("pos_eval") {
        "reposition"
    } else if reason.starts_with("fallback from") {
        "viability_fallback"
    } else if reason.starts_with("no rule matched") {
        "no_rule_default"
    } else {
        "unclassified"
    }
}

/// Build a `PlanLogEntry` from a plan + its raw factors and final score.
/// `chosen_idx` is compared against the plan's own index to set `chosen`.
pub fn plan_to_log_entry<'a>(
    plan: &'a TurnPlan,
    rank: usize,
    chosen: bool,
    raw_factors: [f32; 9],
    score: f32,
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
) -> AiLogEntry<'a> {
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
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_prefixes() {
        assert_eq!(classify_selection("panic: hp%=15%<20% AND danger=0.8>0.6"), "panic_override");
        assert_eq!(classify_selection("hp%=30%<40% × danger=0.50"), "urgency");
        assert_eq!(classify_selection("ally hp%=40%<50% (healer support=0.10)"), "protect_ally");
        assert_eq!(classify_selection("forced by taunt (FORCES_TARGETING)"), "taunt_forced");
        assert_eq!(classify_selection("CC the taunter (threat=7.0)"), "taunt_cc");
        assert_eq!(classify_selection("killable: threat=4.9>=eff_hp=4, reach_budget=3"), "killable");
        assert_eq!(classify_selection("highest priority=0.62"), "best_priority");
        assert_eq!(classify_selection("fallback from ApplyCC: max_align=0.20<threshold=0.50"), "viability_fallback");
        assert_eq!(classify_selection("no rule matched — default reposition"), "no_rule_default");
    }

    #[test]
    fn classify_unknown_prefix_returns_unclassified() {
        assert_eq!(classify_selection("some new rule fired"), "unclassified");
    }

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
