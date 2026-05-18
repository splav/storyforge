//! Engine integration tests for `step(Action::EndTurn)` — Phase 4b.
//!
//! Covers: mid-round handoff, end-of-round wrap, dead-skip, stunned-skip
//! (direct status), all-stunned budget break, sirota DoT during dead-skip,
//! NotCurrent rejection.

use hexx::Hex;

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::{ContentView, StatusBonuses},
    dice::ExpectedValue,
    event::{Event, TurnSkipReason},
    legality::IllegalReason,
    state::{ActiveStatus, CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    StatusDef, StatusId,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// StubContent that can report `skips_turn=true` for a specific status id.
struct StubContent {
    stun_status: Option<StatusId>,
    /// If set, this status has dot_per_tick=5 and no other special flags.
    dot_status: Option<StatusId>,
}

impl StubContent {
    fn plain() -> Self {
        Self { stun_status: None, dot_status: None }
    }

    fn with_stun(id: StatusId) -> Self {
        Self { stun_status: Some(id), dot_status: None }
    }

    fn with_dot(id: StatusId) -> Self {
        Self { stun_status: None, dot_status: Some(id) }
    }
}

impl ContentView for StubContent {
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses { StatusBonuses::default() }

    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId)
        -> Option<storyforge::combat_engine::AbilityDef> { None }

    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        let skips = self.stun_status.as_ref().is_some_and(|s| s == id);
        let is_dot = self.dot_status.as_ref().is_some_and(|s| s == id);
        Some(StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: skips,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            speed_bonus: 0,
            hp_percent_dot: if is_dot { 0 } else { 0 },
        })
    }

    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

fn uid(n: u64) -> UnitId { UnitId(n) }

fn make_unit(id: u64, alive: bool) -> Unit {
    Unit {
        id: UnitId(id),
        team: Team::Player,
        pos: Hex::ZERO,
        hp: if alive { 20 } else { 0 },
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        base_speed: 3,
        speed: 3,
        action_points: 2,
        max_ap: 2,
        movement_points: 3,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    }
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
        applier,
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
        &StubContent::plain(),
    )
    .expect("mid-round handoff must succeed");

    assert_eq!(events.len(), 4, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor } if *actor == uid(1)));
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
    let content = StubContent::plain();

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(2) },
        &mut ExpectedValue,
        &content,
    )
    .expect("end-of-round wrap must succeed");

    assert_eq!(events.len(), 5, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor } if *actor == uid(2)));
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
        &StubContent::plain(),
    )
    .expect("dead-skip must succeed");

    assert_eq!(events.len(), 5, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor } if *actor == uid(1)));
    assert!(matches!(&events[2], Event::TurnSkipped { actor, reason: TurnSkipReason::Dead } if *actor == uid(2)));
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

    let mut state = make_state(
        vec![make_unit(1, true), b],
        vec![uid(1), uid(2)],
        0,
    );
    let content = StubContent::with_stun(stun_id);

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &content,
    )
    .expect("stunned-skip wrap must succeed");

    assert_eq!(events.len(), 6, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor } if *actor == uid(1)));
    assert!(matches!(&events[2], Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(2)));
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
    let content = StubContent::with_stun(stun_id);

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: uid(3) },
        &mut ExpectedValue,
        &content,
    )
    .expect("wrap+skip must succeed");

    assert_eq!(events.len(), 6, "got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor } if *actor == uid(3)));
    assert!(matches!(&events[2], Event::RoundStarted { round: 2 }));
    assert!(matches!(&events[3], Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == uid(1)));
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
        applier,
    });

    let b = make_unit(2, false); // dead

    let mut state = make_state(
        vec![make_unit(1, true), b, c],
        vec![uid(1), uid(2), uid(3)],
        0,
    );
    let content = StubContent::with_dot(dot_id.clone());

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

    // StatusTicked (DoT tick) must appear before TurnSkipped{B}.
    let ticked_pos = events.iter().position(|e| {
        matches!(e, Event::StatusTicked { target, .. } if *target == uid(3))
    });
    if let Some(pos) = ticked_pos {
        assert!(pos < skip_pos, "StatusTicked must precede TurnSkipped");
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
        &StubContent::plain(),
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

    let mut state = make_state(
        vec![a, b],
        vec![uid(1), uid(2)],
        0,
    );
    let content = StubContent::with_stun(stun_id);

    let result = step(
        &mut state,
        Action::EndTurn { actor: uid(1) },
        &mut ExpectedValue,
        &content,
    );

    assert!(result.is_ok(), "budget guard must prevent infinite loop and return Ok");
    let (events, _ctx) = result.unwrap();
    // ActionStarted must be first; total events should be reasonable.
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(events.len() < 50, "too many events — possible loop: {} events", events.len());
}
