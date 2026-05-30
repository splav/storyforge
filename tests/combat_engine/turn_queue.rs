//! Unit tests for `TurnQueue` (engine) and the `CombatState` integration
//! (`set_turn_queue`, `start_round`).
//!
//! Covers:
//! - `TurnQueue` default / new / current / advance / is_empty
//! - wrap-around semantics (including the length-1 edge case)
//! - `wrapped_after` predicate
//! - `CombatState::default` has empty queue
//! - `CombatState::set_turn_queue` sets order and index
//! - `CombatState::start_round` resets reactions, index, and phase

use storyforge::combat_engine::{
    content::ContentView,
    state::{CombatState, RoundPhase, Unit, UnitId},
    turn_queue::TurnQueue,
};

use crate::common::engine_unit::EngineUnitBuilder;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `Some(&STUB_STATUS_DEF)` for every status — used by `start_round`
/// which queries status bonuses on reactions reset. Distinct from StubContent
/// (which returns None for unknown statuses).
struct StubContent;

static STUB_STATUS_DEF: storyforge::combat_engine::StatusDef = storyforge::combat_engine::StatusDef {
    causes_disadvantage: false,
    blocks_mana_abilities: false,
    forces_targeting: false,
    skips_turn: false,
    bonuses: storyforge::combat_engine::StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 0 },
    hp_percent_dot: 0,
};

impl ContentView for StubContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId) -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &storyforge::combat_engine::StatusId) -> Option<&storyforge::combat_engine::StatusDef> {
        Some(&STUB_STATUS_DEF)
    }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

fn uid(n: u64) -> UnitId { UnitId(n) }

/// speed=3, Mp=3, reactions_left=0, hp depends on alive.
fn make_unit(id: UnitId, alive: bool, reactions_max: i32) -> Unit {
    let hp = if alive { 10 } else { 0 };
    EngineUnitBuilder::new(id.0)
        .hp(hp, 10)
        .speed(3)
        .mp(3, 3)
        .reactions(0, reactions_max)
        .build()
}

// ── TurnQueue: default / new / current / is_empty ────────────────────────────

#[test]
fn default_queue_is_empty_and_current_is_none() {
    let q = TurnQueue::default();
    assert!(q.is_empty());
    assert_eq!(q.current(), None);
    assert_eq!(q.index, 0);
}

#[test]
fn new_queue_has_index_zero_and_correct_current() {
    let q = TurnQueue::new(vec![uid(1), uid(2), uid(3)]);
    assert_eq!(q.index, 0);
    assert_eq!(q.current(), Some(uid(1)));
    assert!(!q.is_empty());
}

// ── TurnQueue: advance wrapping ───────────────────────────────────────────────

#[test]
fn advance_steps_through_queue_and_wraps() {
    let mut q = TurnQueue::new(vec![uid(1), uid(2), uid(3)]);
    assert_eq!(q.index, 0);
    q.advance(); assert_eq!(q.index, 1);
    q.advance(); assert_eq!(q.index, 2);
    q.advance(); assert_eq!(q.index, 0); // wrapped
    assert_eq!(q.current(), Some(uid(1)));
}

#[test]
fn advance_on_empty_queue_stays_at_zero() {
    let mut q = TurnQueue::default();
    q.advance();
    assert_eq!(q.index, 0);
    assert_eq!(q.current(), None);
}

// ── TurnQueue: wrapped_after ──────────────────────────────────────────────────

#[test]
fn wrapped_after_true_when_index_less_than_prev() {
    // Queue len 3: advance from index 2 → 0. wrapped_after(2) must be true.
    let mut q = TurnQueue::new(vec![uid(1), uid(2), uid(3)]);
    q.index = 2;
    let prev = q.index;
    q.advance();
    assert_eq!(q.index, 0);
    assert!(q.wrapped_after(prev), "should detect wrap from 2 → 0");
}

#[test]
fn wrapped_after_false_when_index_advanced_within_queue() {
    // Queue len 3: advance from index 0 → 1. No wrap.
    let mut q = TurnQueue::new(vec![uid(1), uid(2), uid(3)]);
    let prev = q.index; // 0
    q.advance();
    assert!(!q.wrapped_after(prev), "0 → 1 is not a wrap");
}

#[test]
fn wrapped_after_on_length_one_queue_always_true() {
    // A singleton queue always "wraps to itself" on every advance.
    // Convention: this counts as a wrap so BumpRound fires every turn —
    // otherwise the single actor would loop forever without incrementing the round.
    let mut q = TurnQueue::new(vec![uid(1)]);
    let prev = q.index; // 0
    q.advance();
    assert_eq!(q.index, 0); // still 0 (modulo 1)
    assert!(
        q.wrapped_after(prev),
        "length-1 queue: every advance is treated as a wrap"
    );
}

#[test]
fn wrapped_after_on_empty_queue_is_true() {
    let q = TurnQueue::default();
    assert!(q.wrapped_after(0));
}

// ── CombatState integration ───────────────────────────────────────────────────

#[test]
fn combat_state_default_has_empty_queue() {
    let state = CombatState::default();
    assert!(state.turn_queue.is_empty());
    assert_eq!(state.turn_queue.current(), None);
}

#[test]
fn set_turn_queue_sets_order_and_index() {
    let mut state = CombatState::default();
    state.set_turn_queue(vec![uid(1), uid(2)], 1);
    assert_eq!(state.turn_queue.index, 1);
    assert_eq!(state.turn_queue.current(), Some(uid(2)));
}

// ── start_round ───────────────────────────────────────────────────────────────

#[test]
fn start_round_sets_index_zero_and_phase_actor_turn() {
    let unit = make_unit(uid(1), true, 1);
    let mut state = CombatState::new(vec![unit], 1, RoundPhase::PreRound, 0);
    state.set_turn_queue(vec![uid(1)], 0);

    let events = state.start_round(&StubContent);

    assert_eq!(state.turn_queue.index, 0);
    assert_eq!(state.phase, RoundPhase::ActorTurn);
    assert!(events.is_empty(), "Phase 4a: start_round emits no events");
}

#[test]
fn start_round_resets_reactions_for_alive_units_only() {
    let alive = make_unit(uid(1), true,  2); // reactions_left starts at 0, max=2
    let dead  = make_unit(uid(2), false, 1); // dead, reactions_left starts at 0

    let mut state = CombatState::new(vec![alive, dead], 1, RoundPhase::PreRound, 0);
    state.set_turn_queue(vec![uid(1), uid(2)], 0);

    state.start_round(&StubContent);

    // Alive unit: reactions_left reset to reactions_max.
    assert_eq!(
        state.unit(uid(1)).unwrap().reactions_left,
        2,
        "alive unit reactions_left should equal reactions_max"
    );
    // Dead unit: reactions_left stays at 0.
    assert_eq!(
        state.unit(uid(2)).unwrap().reactions_left,
        0,
        "dead unit reactions_left should remain 0"
    );
}

#[test]
fn start_round_resets_queue_index_even_when_advanced() {
    // Multi-unit queue at a non-zero index must be reset by start_round.
    let u2 = make_unit(uid(2), true, 1);
    let mut state2 = CombatState::new(vec![make_unit(uid(1), true, 1), u2], 1, RoundPhase::PreRound, 0);
    state2.set_turn_queue(vec![uid(1), uid(2)], 1);
    assert_eq!(state2.turn_queue.index, 1);

    state2.start_round(&StubContent);

    assert_eq!(state2.turn_queue.index, 0, "start_round must reset index to 0");
}
