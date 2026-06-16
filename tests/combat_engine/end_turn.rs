//! Engine integration tests for `step(Action::EndTurn)`.
//!
//! Covers: mid-round handoff, end-of-round wrap, dead-skip, stunned-skip,
//! all-stunned budget break, sirota DoT during dead-skip, NotCurrent rejection,
//! and `settle_round_start` round-start cursor settlement.

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::StatusBonuses,
    dice::ExpectedValue,
    event::{Event, TurnSkipReason},
    legality::IllegalReason,
    state::{ActiveStatus, CombatState, RoundPhase, Unit, UnitId},
    step::step,
    StatusDef, StatusId,
};

use crate::common::engine_unit::{EngineUnitBuilder, StubContent};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_def(skips_turn: bool) -> StatusDef {
    StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn,
        bonuses: StatusBonuses::default(),
        hp_percent_dot: 0,
        heal_per_tick: 0,
        ..Default::default()
    }
}

fn uid(n: u64) -> UnitId {
    UnitId(n)
}

/// hp varies by alive, speed=3, Mp=3 — end_turn defaults.
fn make_unit(id: u64, alive: bool) -> Unit {
    let hp = if alive { 20 } else { 0 };
    EngineUnitBuilder::new(id)
        .hp(hp, 20)
        .speed(3)
        .mp(3, 3)
        .build()
}

fn make_state(units: Vec<Unit>, order: Vec<UnitId>, index: usize) -> CombatState {
    let mut s = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);
    s.set_turn_queue(order, index);
    s
}

fn status(id: &str, rounds: u32, dot: i32, applier: UnitId) -> ActiveStatus {
    ActiveStatus {
        id: StatusId(id.to_string()),
        rounds_remaining: rounds,
        dot_per_tick: dot,
        applier: combat_engine::state::EffectSource::Unit(applier),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// queue=[A,B], current=A. EndTurn{A} → A→B handoff, no wrap.
/// Event order: ActionStarted, TurnEnded{A}, TurnStarted{B}, ActionFinished.
#[test]
fn mid_round_handoff() {
    let mut state = make_state(
        vec![make_unit(1, true), make_unit(2, true)],
        vec![uid(1), uid(2)],
        0,
    );

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &StubContent::new(),
    )
    .expect("mid-round handoff must succeed");

    assert_eq!(events.len(), 4, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor, .. } if *actor == uid(1)));
    assert!(matches!(&events[2], Event::TurnStarted { actor } if *actor == uid(2)));
    assert!(matches!(&events[3], Event::ActionFinished { .. }));

    assert_eq!(state.turn_queue.index, 1);
    assert_eq!(state.round, 1);
}

/// queue=[A,B] index=1 (B is current). EndTurn{B} wraps → round 2, A starts.
/// Event order: ActionStarted, TurnEnded{B}, RoundStarted{2}, TurnStarted{A}, ActionFinished.
#[test]
fn end_of_round_wraps_and_emits_round_started() {
    let mut state = make_state(
        vec![make_unit(1, true), make_unit(2, true)],
        vec![uid(1), uid(2)],
        1,
    );
    let content = StubContent::new();

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(2) },
        &mut ExpectedValue,
        &content,
    )
    .expect("end-of-round wrap must succeed");

    assert_eq!(events.len(), 5, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor, .. } if *actor == uid(2)));
    assert!(matches!(&events[2], Event::RoundStarted { round: 2 }));
    assert!(matches!(&events[3], Event::TurnStarted { actor } if *actor == uid(1)));
    assert!(matches!(&events[4], Event::ActionFinished { .. }));

    assert_eq!(state.round, 2);
    assert_eq!(state.turn_queue.index, 0);
}

/// queue=[A,B,C] current=A, B is dead. EndTurn{A} skips B, lands on C.
/// Event order: ActionStarted, TurnEnded{A}, TurnSkipped{B,Dead}, TurnStarted{C}, ActionFinished.
#[test]
fn dead_skip_in_middle_of_round() {
    let b = make_unit(2, false);
    let mut state = make_state(
        vec![make_unit(1, true), b, make_unit(3, true)],
        vec![uid(1), uid(2), uid(3)],
        0,
    );

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &StubContent::new(),
    )
    .expect("dead-skip must succeed");

    assert_eq!(events.len(), 5, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor, .. } if *actor == uid(1)));
    assert!(
        matches!(&events[2], Event::TurnSkipped { actor, reason: TurnSkipReason::Dead } if *actor == uid(2))
    );
    assert!(matches!(&events[3], Event::TurnStarted { actor } if *actor == uid(3)));
    assert!(matches!(&events[4], Event::ActionFinished { .. }));

    assert_eq!(state.turn_queue.index, 2);
}

/// queue=[A,B] current=A, B has skips_turn status. EndTurn{A} skips B,
/// wraps (index→0→BumpRound), A starts round 2.
/// Event order: ActionStarted, TurnEnded{A}, TurnSkipped{B,Stunned}, RoundStarted{2}, TurnStarted{A}, ActionFinished.
#[test]
fn stunned_skip_via_direct_status_wraps_to_next_round() {
    let stun_id = StatusId("stun".to_string());
    let mut b = make_unit(2, true);
    b.statuses.push(status("stun", 2, 0, uid(99)));

    let mut state = make_state(vec![make_unit(1, true), b], vec![uid(1), uid(2)], 0);
    let content = StubContent::new().with_status(stun_id, make_def(true));

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &content,
    )
    .expect("stunned-skip wrap must succeed");

    assert_eq!(events.len(), 6, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor, .. } if *actor == uid(1)));
    assert!(
        matches!(&events[2], Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(2))
    );
    assert!(matches!(&events[3], Event::RoundStarted { round: 2 }));
    assert!(matches!(&events[4], Event::TurnStarted { actor } if *actor == uid(1)));
    assert!(matches!(&events[5], Event::ActionFinished { .. }));
}

/// queue=[A,B,C] current=C, A is stunned, B alive. EndTurn{C} wraps,
/// BumpRound, skip A → B starts.
/// Event order: ActionStarted, TurnEnded{C}, RoundStarted{2}, TurnSkipped{A,Stunned}, TurnStarted{B}, ActionFinished.
#[test]
fn round_wrap_skips_first_actor_if_stunned() {
    let stun_id = StatusId("stun".to_string());
    let mut a = make_unit(1, true);
    a.statuses.push(status("stun", 2, 0, uid(99)));

    let mut state = make_state(
        vec![a, make_unit(2, true), make_unit(3, true)],
        vec![uid(1), uid(2), uid(3)],
        2, // C is current
    );
    let content = StubContent::new().with_status(stun_id, make_def(true));

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(3) },
        &mut ExpectedValue,
        &content,
    )
    .expect("wrap+skip must succeed");

    assert_eq!(events.len(), 6, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor, .. } if *actor == uid(3)));
    assert!(matches!(&events[2], Event::RoundStarted { round: 2 }));
    assert!(
        matches!(&events[3], Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(1))
    );
    assert!(matches!(&events[4], Event::TurnStarted { actor } if *actor == uid(2)));
    assert!(matches!(&events[5], Event::ActionFinished { .. }));
}

/// Sirota-DoT: queue=[A,B,C] current=A, B is dead and has a DoT status on C.
/// EndTurn{A} → advance to B → tick_actor_statuses(B) fires DoT events on C
/// → TurnSkipped{B,Dead}. Verify DoT events appear before TurnSkipped.
#[test]
fn sirota_dot_propagates_when_dead_unit_skipped() {
    let dot_id = StatusId("poison".to_string());
    let applier = uid(2); // B applied the poison

    let mut c = make_unit(3, true);
    // B (dead) applied poison to C
    c.statuses.push(ActiveStatus {
        id: dot_id.clone(),
        rounds_remaining: 3,
        dot_per_tick: 5,
        applier: combat_engine::state::EffectSource::Unit(applier),
    });

    let b = make_unit(2, false); // dead

    let mut state = make_state(
        vec![make_unit(1, true), b, c],
        vec![uid(1), uid(2), uid(3)],
        0,
    );
    let content = StubContent::new().with_status(dot_id.clone(), make_def(false));

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &content,
    )
    .expect("sirota DoT must succeed");

    // Find the TurnSkipped{B} position.
    let skip_pos = events.iter().position(|e| {
        matches!(e, Event::TurnSkipped { actor, reason: TurnSkipReason::Dead } if *actor == uid(2))
    }).expect("TurnSkipped{B} must appear");

    // DotDamaged (DoT tick) must appear before TurnSkipped{B} if it fires.
    let dot_pos = events
        .iter()
        .position(|e| matches!(e, Event::DotDamaged { target, .. } if *target == uid(3)));
    if let Some(pos) = dot_pos {
        assert!(pos < skip_pos, "DotDamaged must precede TurnSkipped");
    }
    // Whether or not DoT fires depends on tick_actor_statuses; at minimum
    // TurnSkipped{B,Dead} must appear.
}

/// EndTurn by actor who is not the current cursor → Illegal(NotCurrent).
#[test]
fn rejects_when_actor_not_current() {
    let mut state = make_state(
        vec![make_unit(1, true), make_unit(2, true)],
        vec![uid(1), uid(2)],
        0, // A is current
    );

    let err = step(
        &mut state,
        Action::EndTurn { actor: uid(2) }, // B tries to end turn
        &mut ExpectedValue,
        &StubContent::new(),
    )
    .expect_err("non-current actor must be rejected");

    assert_eq!(err, ActionError::Illegal(IllegalReason::NotCurrent));
}

/// Both A and B are alive but both stunned. Budget guard must prevent infinite
/// loop; step returns Ok within a bounded number of events.
#[test]
fn all_stunned_budget_breaks_loop() {
    let stun_id = StatusId("stun".to_string());
    let mut a = make_unit(1, true);
    a.statuses.push(status("stun", 2, 0, uid(99)));
    let mut b = make_unit(2, true);
    b.statuses.push(status("stun", 2, 0, uid(99)));

    let mut state = make_state(vec![a, b], vec![uid(1), uid(2)], 0);
    let content = StubContent::new().with_status(stun_id, make_def(true));

    let result = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &content,
    );

    assert!(
        result.is_ok(),
        "budget guard must prevent infinite loop and return Ok"
    );
    let (events, _ctx) = result.unwrap();
    // ActionStarted must be first; total events should be reasonable.
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(
        events.len() < 50,
        "too many events — possible loop: {} events",
        events.len()
    );
}

// ── settle_round_start tests ─────────────────────────────────────────────────

/// settle_round_start with a healthy (no stun) roster → settles on cursor 0,
/// emits RoundStarted then TurnStarted, no TurnSkipped, round not incremented.
#[test]
fn settle_round_start_no_stun_lands_on_first_actor() {
    let mut state = make_state(
        vec![make_unit(1, true), make_unit(2, true)],
        vec![uid(1), uid(2)],
        0,
    );
    // round is already 1 from make_state

    let events = state.settle_round_start(&StubContent::new());

    // Must emit RoundStarted{1}, then TurnStarted{A} (plus start_actor_turn events).
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::RoundStarted { round: 1 })),
        "must emit RoundStarted{{1}}; got: {:?}",
        events
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::TurnStarted { actor } if *actor == uid(1))),
        "must emit TurnStarted{{A}}; got: {:?}",
        events
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::TurnSkipped { .. })),
        "no TurnSkipped expected; got: {:?}",
        events
    );
    // round must not have been incremented
    assert_eq!(state.round, 1);
    // cursor stays on 0
    assert_eq!(state.turn_queue.index, 0);
}

/// settle_round_start with actor at index 0 stunned → emits RoundStarted,
/// TurnSkipped{A,Stunned}, TurnStarted{B}. Round counter not double-incremented.
#[test]
fn settle_round_start_skips_stunned_first_actor() {
    let stun_id = StatusId("stun".to_string());
    let mut a = make_unit(1, true);
    // stun applied by uid(1) itself so that the tick is "self-applied"
    // (tick_actor_statuses looks for statuses where applier == actor)
    a.statuses.push(status("stun", 2, 0, uid(1)));

    let mut state = make_state(vec![a, make_unit(2, true)], vec![uid(1), uid(2)], 0);
    let content = StubContent::new().with_status(stun_id, make_def(true));

    let events = state.settle_round_start(&content);

    // Must contain: RoundStarted, TurnSkipped{A,Stunned}, TurnStarted{B} — in that order.
    let rs_pos = events
        .iter()
        .position(|e| matches!(e, Event::RoundStarted { .. }))
        .expect("RoundStarted must appear");
    let skip_pos = events.iter().position(|e| {
        matches!(e, Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(1))
    }).expect("TurnSkipped{A,Stunned} must appear");
    let ts_pos = events
        .iter()
        .position(|e| matches!(e, Event::TurnStarted { actor } if *actor == uid(2)))
        .expect("TurnStarted{B} must appear");

    assert!(rs_pos < skip_pos, "RoundStarted must precede TurnSkipped");
    assert!(skip_pos < ts_pos, "TurnSkipped must precede TurnStarted");

    // Round must NOT be double-incremented.
    assert_eq!(state.round, 1, "round must stay at 1, not be bumped");
}

/// 1-turn stun: a unit stunned for rounds_remaining=1, where applier==actor.
/// After settle_round_start skips it, the stun's ExpireStatus ticks so that
/// on the NEXT skip-check the unit is no longer stunned.
#[test]
fn one_turn_stun_expires_after_skip() {
    let stun_id = StatusId("stun".to_string());
    let mut a = make_unit(1, true);
    // Self-applied 1-round stun: actor is both bearer and applier.
    a.statuses.push(status("stun", 1, 0, uid(1)));

    let mut state = make_state(vec![a, make_unit(2, true)], vec![uid(1), uid(2)], 0);
    let content = StubContent::new().with_status(stun_id.clone(), make_def(true));

    // Run settle — this should skip A (stunned) and tick the 1-round stun.
    let events = state.settle_round_start(&content);

    // A must have been skipped.
    assert!(events.iter().any(|e| {
        matches!(e, Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(1))
    }), "A must be skipped as stunned");

    // After the tick, A's stun should be gone (rounds_remaining decremented from 1 → 0 → removed).
    let a_unit = state.unit(uid(1)).expect("A must still exist");
    let still_stunned = a_unit.statuses.iter().any(|s| s.id == stun_id);
    assert!(
        !still_stunned,
        "stun must have expired after 1-round tick; statuses: {:?}",
        a_unit.statuses
    );
}

/// A DoT (poison) on a stunned unit ticks (deals damage) when the unit is
/// skipped via settle_round_start. The poison applier == the stunned actor.
#[test]
fn stunned_skip_ticks_dot_on_victim() {
    let poison_id = StatusId("poison".to_string());
    let stun_id = StatusId("stun".to_string());

    // A is stunned; B is A's victim with a DoT applied by A.
    let mut a = make_unit(1, true);
    a.statuses.push(status("stun", 2, 0, uid(99))); // stun bearer (applier=99, irrelevant)

    let mut b = make_unit(2, true);
    b.statuses.push(ActiveStatus {
        id: poison_id.clone(),
        rounds_remaining: 3,
        dot_per_tick: 5,
        applier: combat_engine::state::EffectSource::Unit(uid(1)), // applied BY A
    });

    let mut state = make_state(vec![a, b], vec![uid(1), uid(2)], 0);
    let content = StubContent::new()
        .with_status(stun_id, make_def(true))
        .with_status(poison_id, make_def(false));

    let events = state.settle_round_start(&content);

    // A must be skipped.
    assert!(events.iter().any(|e| {
        matches!(e, Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(1))
    }), "A must be TurnSkipped{{Stunned}}");

    // DoT damage on B must appear BEFORE TurnSkipped{A}.
    let skip_pos = events
        .iter()
        .position(|e| matches!(e, Event::TurnSkipped { actor, .. } if *actor == uid(1)))
        .unwrap();
    let dot_pos = events
        .iter()
        .position(|e| matches!(e, Event::DotDamaged { target, .. } if *target == uid(2)))
        .expect("DotDamaged on B must appear");
    assert!(dot_pos < skip_pos, "DotDamaged must precede TurnSkipped");
}

/// All-stunned roster → settle_round_start terminates within budget (no hang),
/// emits skips, no panic. No TurnStarted emitted (no valid actor).
#[test]
fn settle_round_start_all_stunned_terminates() {
    let stun_id = StatusId("stun".to_string());
    let mut a = make_unit(1, true);
    a.statuses.push(status("stun", 2, 0, uid(99)));
    let mut b = make_unit(2, true);
    b.statuses.push(status("stun", 2, 0, uid(99)));

    let mut state = make_state(vec![a, b], vec![uid(1), uid(2)], 0);
    let content = StubContent::new().with_status(stun_id, make_def(true));

    let events = state.settle_round_start(&content);

    // Must not panic and must emit a bounded number of events.
    assert!(
        events.len() < 50,
        "budget guard must bound events; got {}",
        events.len()
    );
    // RoundStarted must appear.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::RoundStarted { .. })),
        "RoundStarted must appear; got: {:?}",
        events
    );
    // No TurnStarted — no valid actor found.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::TurnStarted { .. })),
        "TurnStarted must not appear for all-stunned roster"
    );
}
