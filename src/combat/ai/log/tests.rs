//! Tests for `mod.rs` (log) — split from the source file via `#[path]` in
//! `mod.rs` (see end of that file). Production code stays in `mod.rs`; this
//! file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../docs/testing.md):
//! `log/mod.rs` grew to 2161 LOC with tests dominating the lower half.
//!
//! `super::*` here resolves to `log/mod.rs` (since this file is included
//! as `mod tests` inside mod.rs).

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
fn build_combat_log_dir_shape() {
    let d = build_combat_log_dir("main", "scene1", "goblin_camp", 1_776_609_022);
    assert_eq!(
        d,
        PathBuf::from("logs").join("20260419T143022_main_scene1_goblin_camp")
    );
    // Folder name == session_id (no .jsonl extension).
    let name = d.file_name().unwrap().to_string_lossy();
    assert_eq!(name, "20260419T143022_main_scene1_goblin_camp");
}

/// Gate #11 / #12: `CombatLogHeader` serializes with `event_type =
/// "combat_log_header"` and is skipped by the `actor_tick` filter used
/// in `mine_ai_logs`.
#[test]
fn combat_log_header_serializes_and_is_skipped_by_miner_filter() {
    let header = CombatLogHeader {
        event_type: "combat_log_header",
        schema_version: SCHEMA_VERSION,
        session_id: "20260419T143022_main_scene1_goblin_camp",
    };
    let json = serde_json::to_string(&header).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["event_type"], "combat_log_header");
    assert_eq!(v["schema_version"], SCHEMA_VERSION);
    assert_eq!(v["session_id"], "20260419T143022_main_scene1_goblin_camp");
    // mine_ai_logs filter: skip non-"actor_tick" events.
    assert_ne!(
        v.get("event_type").and_then(|t| t.as_str()),
        Some("actor_tick"),
        "header must be skipped by actor_tick filter"
    );
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
        session_id: "test_session",
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
        chosen_intent: None,
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
    assert_eq!(
        event.continuation.is_none(),
        restored.continuation.is_none()
    );
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
        "expected Skip, got {:?}",
        event.decision
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
        session_id: "test_session",
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
        chosen_intent: None,
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
    assert!(
        event.plans[0].annotation.chosen,
        "rank 1 plan is the chosen one"
    );
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
    plan.annotation
        .outcomes
        .push(ActionOutcomeEstimate::default());

    let mut pool = ScoredPool::new(vec![plan]);
    pool.annotations[0].score = 1.0;
    pool.annotations[0].chosen = true;

    let snap = BattleSnapshot::default();
    let debug_names = std::collections::HashMap::new();
    let actor = Entity::from_bits(7);
    let decision = AiDecision::EndTurn;
    let reason = crate::combat::ai::intent::IntentReason::NoRuleDefault;
    let input = ActorTickInput {
        session_id: "test_session",
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
        chosen_intent: None,
        debug_names: &debug_names,
        status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
        band: None,
        agenda: None,
    };
    let event = build_actor_tick_event(input);

    assert_eq!(event.plans.len(), 1);
    assert_eq!(
        event.plans[0].annotation.outcomes.len(),
        1,
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
        matches!(result, Err(LogError::UnsupportedSchema { found: 27, .. })),
        "v27 must produce UnsupportedSchema(found=27), got: {result:?}",
    );
}

/// v26 log line also returns `UnsupportedSchema` error.
#[test]
fn parse_v26_returns_unsupported_schema_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":26}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 26, .. })),
        "v26 must produce UnsupportedSchema(found=26), got: {result:?}",
    );
}

/// v28 log line returns `UnsupportedSchema` — wire format break (raw_factors → factors named map).
#[test]
fn actor_tick_v28_load_yields_unsupported_schema_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":28}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 28, .. })),
        "v28 must produce UnsupportedSchema(found=28), got: {result:?}",
    );
}

/// v29 log line returns `UnsupportedSchema` — step 9.B bump (actor_statuses fields added in v30).
#[test]
fn actor_tick_v29_load_yields_unsupported_schema_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":29}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 29, .. })),
        "v29 must produce UnsupportedSchema(found=29), got: {result:?}",
    );
}

/// v30 log line returns `UnsupportedSchema` — step 10.4 bump (critics serialised, SanityRule shrunk).
#[test]
fn actor_tick_v30_load_yields_unsupported_schema_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":30}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 30, .. })),
        "v30 must produce UnsupportedSchema(found=30), got: {result:?}",
    );
}

/// v31 log line returns `UnsupportedSchema` — step 11.6 bump (bands/agenda serialisation in v32).
#[test]
fn actor_tick_v31_load_yields_unsupported_schema_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":31}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 31, .. })),
        "v31 must produce UnsupportedSchema(found=31), got: {result:?}",
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
    let parsed =
        parse_actor_tick(&json).expect("parse_actor_tick should succeed for current schema");
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
        urgency: 0.8,
        feasibility: 0.9,
        leverage: 0.7,
        safety: 0.6,
        role_affinity: 0.5,
        continuation_value: 0.4,
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
    let agenda = Agenda {
        band,
        items: vec![agenda_item],
    };

    let input = ActorTickInput {
        session_id: "test_session",
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
        chosen_intent: None,
        debug_names: &debug_names,
        status_tags: crate::combat::ai::test_helpers::empty_status_tag_cache(),
        band: Some((band, band_reason)),
        agenda: Some(&agenda),
    };

    let event = build_actor_tick_event(input);
    // schema_version is now 33 (P3b bump).
    assert_eq!(
        event.schema_version, SCHEMA_VERSION,
        "must be current schema version"
    );
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
    assert!(
        logged_ann.score_trace_log.is_some(),
        "score_trace_log must be present after P3b"
    );

    // Full JSON round-trip at current schema.
    let json = serde_json::to_string(&event).expect("serialize");
    let restored: ActorTickEvent =
        parse_actor_tick(&json).expect("parse_actor_tick current schema");
    assert_eq!(restored.schema_version, SCHEMA_VERSION);
    assert_eq!(restored.band, Some(PriorityBand::NormalTactical));
    assert_eq!(restored.agenda.len(), 1);
    assert_eq!(restored.plans[0].annotation.agenda_item, Some(0u8));
    assert_eq!(
        restored.plans[0].annotation.considerations_per_item.len(),
        1
    );
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
        matches!(result, Err(LogError::UnsupportedSchema { found: 32, .. })),
        "v32 must produce UnsupportedSchema(found=32), got: {result:?}",
    );
}

/// v33 corpus is now rejected (Phase A3 clean break — MIN_SUPPORTED = 37).
#[test]
fn schema_v33_rejected_as_pre_v37_clean_break() {
    let json = r#"{
            "event_type": "actor_tick",
            "schema_version": 33,
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
        matches!(result, Err(LogError::UnsupportedSchema { found: 33, .. })),
        "v33 must produce UnsupportedSchema(found=33, required=45) after Phase A3, got: {result:?}",
    );
}

/// v31 format must yield UnsupportedSchema with a hint mentioning v32 score_trace_log.
#[test]
fn schema_v31_rejected_with_clear_error() {
    let json = r#"{"event_type":"actor_tick","schema_version":31,"round":1,"timestamp_ms":0,"actor_id":1,"actor_name":"x","plans":[],"decision":{"kind":"end_turn"},"snapshot":{}}"#;
    let result = parse_actor_tick(json);
    let Err(LogError::UnsupportedSchema {
        found,
        required,
        hint: _,
    }) = result
    else {
        panic!("expected UnsupportedSchema, got: {result:?}");
    };
    assert_eq!(found, 31);
    // Tie to the constant — the error echoes the current schema, not a literal.
    assert_eq!(required, SCHEMA_VERSION);
}

/// v35 log is now rejected (Phase A3 clean break — MIN_SUPPORTED = 37).
/// The v33–v35 base_speed reconstructor was removed along with the migration.
#[test]
fn v35_log_rejected_as_pre_v37_clean_break() {
    let json = r#"{"event_type":"actor_tick","schema_version":35}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 35, .. })),
        "v35 must produce UnsupportedSchema(found=35, required=45) after Phase A3, got: {result:?}",
    );
}

/// v42 log is now rejected (Wave 1 ch2 schema bump: MIN_SUPPORTED = 43).
/// `CombatState.blocked_hexes` added in v43 — clean break.
#[test]
fn parse_actor_tick_v42_returns_unsupported_schema() {
    let json = r#"{"event_type":"actor_tick","schema_version":42}"#;
    let result = parse_actor_tick(json);
    assert!(
        matches!(result, Err(LogError::UnsupportedSchema { found: 42, .. })),
        "v42 must produce UnsupportedSchema(found=42), got: {result:?}",
    );
}
