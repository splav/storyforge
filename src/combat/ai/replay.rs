//! Offline replay pipeline — shared between the `replay_ai_log` binary and
//! integration tests (`tests/ai_scenarios.rs`, `tests/replay_assert.rs`).
//!
//! - **Assertion pipeline** ([`assert_v28_log_file`]) reads a v28 `ActorTickEvent`
//!   from a JSONL log, re-runs the production `pick_action` on its snapshot,
//!   reconstructs the chosen decision via
//!   [`replay_assertion::build_actual_decision`], and compares it against
//!   an overlay loaded from `*.expected.toml`. Returns an [`AssertOutcome`]
//!   with both the raw decision and the pass/fail verdict.
//!
//! Tests call [`assert_v28_log_file`] directly (no subprocess); the
//! `replay_ai_log` binary wraps it with CLI-level I/O and exit codes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use bevy::prelude::Entity;
use serde::{Deserialize, Serialize};

use crate::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use crate::combat::ai::replay_assertion::{
    build_actual_decision, run_assertion, ActualDecision, AssertResult, Overlay,
};
use crate::content::content_view::ContentView;
use crate::core::DiceRng;

// ── Assert pipeline ──────────────────────────────────────────────────────────

/// Outcome of running [`assert_v28_log_file`] — contains both the reconstructed
/// decision and the pass/fail verdict. Non-`Fail` does not imply the test
/// logically passed; inspect [`AssertOutcome::result`].
#[derive(Debug)]
pub struct AssertOutcome {
    pub jsonl_path: PathBuf,
    pub overlay_path: PathBuf,
    /// Always 0 for v27 logs (v27 logs do not have a plan_id field).
    pub plan_id: u64,
    /// Index of the chosen plan within the scored pool.
    pub chosen_idx: usize,
    /// Schema version of the log entry.
    pub schema_version: u32,
    /// Actor entity bits from the log entry.
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
/// production `pick_action` pipeline made for one `ActorTickEvent`. Used by
/// `--capture-golden` / `--compare-golden` in `replay_ai_log`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoldenRecord {
    /// Source JSONL path (as provided on the CLI, for corpus matching).
    pub log_path: String,
    /// Always 0 for v28 logs (preserved for JSON format stability).
    pub plan_id: u64,
    /// Actor entity bits from `ActorTickEvent.actor_id`.
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

// ── v28 assert pipeline ───────────────────────────────────────────────────────

/// Load overlay, locate the targeted `ActorTickEvent`, run the production
/// `pick_action` pipeline, and compare the result against overlay expectations.
///
/// Reads the first non-skip `ActorTickEvent` from `jsonl_path` (or the event
/// whose `actor_id` equals the `plan_id` in the overlay scope, when specified —
/// see note below), runs the production `pick_action` on its snapshot, and
/// compares the result against the overlay expectations.
///
/// **Note on `plan_id` in the overlay scope**: v28 logs do not have a `plan_id`
/// field. The overlay's `[scope].plan_id` is reinterpreted as the target `actor_id`
/// (entity bits) for v28 files. When absent the first non-skip event is used.
/// This matches how the existing `ai_scenarios` overlays use `plan_id` to select
/// a specific entry.
///
/// Returns `AssertError::EntryParse` on schema version mismatch (v27 logs
/// give a clear `UnsupportedSchema` message).
pub fn assert_v28_log_file(
    jsonl_path: &Path,
    overlay_path: &Path,
    content: &ContentView,
    inf_cfg: &InfluenceConfig,
) -> Result<AssertOutcome, AssertError> {
    use crate::combat::ai::log::{parse_actor_tick, ActorTickEvent, LoggedDecision};
    use crate::combat::ai::utility::pick_action;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::utility::AiWorld;
    use crate::combat::ai::difficulty::DifficultyProfile;

    let overlay = load_overlay(overlay_path)?;
    // In v28 we reinterpret plan_id as actor_id for entry selection.
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

        // Skip non-actor_tick lines.
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
                continue;
            }
        }

        // Use parse_actor_tick for schema-versioned parsing.
        let event: ActorTickEvent = match parse_actor_tick(&line) {
            Ok(e) => e,
            Err(e) => {
                // Convert LogError to AssertError by wrapping as a serde error.
                // For UnsupportedSchema, re-create a descriptive serde error string.
                let msg = e.to_string();
                return Err(AssertError::EntryParse {
                    path: jsonl_path.to_path_buf(),
                    source: serde_json::from_str::<serde_json::Value>(&format!("{msg:?}"))
                        .unwrap_err(),
                });
            }
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
    let memory = build_memory_from_overlay(&overlay);
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
        plan_id: 0, // v28 logs have no plan_id
        chosen_idx,
        schema_version: event.schema_version,
        actor_id: event.actor_id,
        actual,
        result: assert_result,
    })
}

// ── AiMemory overlay injection ────────────────────────────────────────────────

/// Parse a `u64` from an optional string field in the overlay.
///
/// TOML integers are limited to `i64` range. Large `u64` values (e.g. status
/// hashes) must be specified as decimal or hex (`"0x..."`) strings. Returns 0
/// when the field is absent.
fn parse_u64_field(value: Option<&str>, field_name: &'static str) -> u64 {
    let Some(s) = value else { return 0; };
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .unwrap_or_else(|e| panic!("invalid hex u64 in overlay field {field_name}: {e}"))
    } else {
        s.parse::<u64>()
            .unwrap_or_else(|e| panic!("invalid decimal u64 in overlay field {field_name}: {e}"))
    }
}

/// Build an `AiMemory` from the overlay's optional `[ai_memory]` section.
///
/// When the section is absent, or when `last_goal_kind` is not provided,
/// returns `AiMemory::default()` (no stored goal). When `last_goal_kind` is
/// present, constructs a `StoredGoalContext` from the flat overlay fields.
fn build_memory_from_overlay(overlay: &Overlay) -> crate::combat::ai::intent::AiMemory {
    use crate::combat::ai::intent::AiMemory;
    use crate::combat::ai::repair::{GoalKind, StoredGoalContext};
    use crate::game::hex::Hex;

    let Some(mem_overlay) = &overlay.ai_memory else {
        return AiMemory::default();
    };
    let Some(kind_str) = &mem_overlay.last_goal_kind else {
        return AiMemory::default();
    };

    let target_bits = mem_overlay.last_goal_target;
    let region_anchor_raw = mem_overlay.last_goal_region_anchor.unwrap_or([0, 0]);
    let region_anchor = Hex { x: region_anchor_raw[0], y: region_anchor_raw[1] };

    let kind = match kind_str.as_str() {
        "Finish" => {
            let bits = target_bits.expect("Finish goal requires last_goal_target");
            let target = bevy::prelude::Entity::try_from_bits(bits)
                .expect("invalid last_goal_target entity bits");
            GoalKind::Finish { target }
        }
        "Pressure" => {
            let bits = target_bits.expect("Pressure goal requires last_goal_target");
            let target = bevy::prelude::Entity::try_from_bits(bits)
                .expect("invalid last_goal_target entity bits");
            GoalKind::Pressure { target }
        }
        "DisableEnemy" => {
            let bits = target_bits.expect("DisableEnemy goal requires last_goal_target");
            let target = bevy::prelude::Entity::try_from_bits(bits)
                .expect("invalid last_goal_target entity bits");
            GoalKind::DisableEnemy { target }
        }
        "HealAlly" => {
            let bits = target_bits.expect("HealAlly goal requires last_goal_target");
            let ally = bevy::prelude::Entity::try_from_bits(bits)
                .expect("invalid last_goal_target entity bits");
            GoalKind::HealAlly { ally }
        }
        "Retreat" => GoalKind::Retreat { region_anchor },
        "SetupAOE" => {
            let ability = mem_overlay.last_goal_planned_ability.clone()
                .expect("SetupAOE goal requires last_goal_planned_ability");
            GoalKind::SetupAOE { region_center: region_anchor, planned_ability: ability.into() }
        }
        "Reposition" => GoalKind::Reposition { region_center: region_anchor },
        other => panic!("unknown last_goal_kind in overlay: {other:?}"),
    };

    let expected_pos_raw = mem_overlay.last_goal_expected_actor_pos.unwrap_or([0, 0]);
    let target_pos_raw = mem_overlay.last_goal_target_pos_at_store.unwrap_or([0, 0]);

    let actor_status_hash = parse_u64_field(
        mem_overlay.last_goal_actor_status_hash.as_deref(), "last_goal_actor_status_hash",
    );

    let stored = StoredGoalContext {
        kind,
        region_anchor,
        region_radius: mem_overlay.last_goal_region_radius.unwrap_or(2),
        planned_ability: mem_overlay.last_goal_planned_ability.as_ref()
            .map(|s| s.clone().into()),
        ttl: mem_overlay.last_goal_ttl.unwrap_or(2),
        confidence: mem_overlay.last_goal_confidence.unwrap_or(1.0),
        created_round: mem_overlay.last_goal_created_round.unwrap_or(0),
        expected_actor_pos: Hex { x: expected_pos_raw[0], y: expected_pos_raw[1] },
        actor_hp_at_store: mem_overlay.last_goal_actor_hp_at_store.unwrap_or(0),
        actor_rage_at_store: mem_overlay.last_goal_actor_rage_at_store.unwrap_or(0),
        actor_status_hash,
        target_hp_at_store: mem_overlay.last_goal_target_hp_at_store.unwrap_or(0),
        target_pos_at_store: Hex { x: target_pos_raw[0], y: target_pos_raw[1] },
    };

    AiMemory {
        last_goal: Some(stored),
        ..AiMemory::default()
    }
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
