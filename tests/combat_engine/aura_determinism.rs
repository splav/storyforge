//! Verifies that `aura_membership_set` returns a BTreeSet with stable iteration
//! order across 10 calls on the same state (Phase 5 gate §7 item 3).

use storyforge::combat_engine::{AuraDef, StatusId, TeamRelation};
use storyforge::combat_engine::state::{CombatState, RoundPhase, Team, Unit, UnitId};
use hexx::Hex;

use crate::common::engine_unit::{EngineUnitBuilder, StubContent};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId { UnitId(n) }
fn sid(s: &str) -> StatusId { StatusId(s.to_string()) }

/// hp=10, speed=3, Mp=3 — aura_determinism defaults.
fn make_unit(id: u64, team: Team, pos: Hex) -> Unit {
    EngineUnitBuilder::new(id)
        .team(team)
        .pos_hex(pos)
        .hp_full(10)
        .speed(3)
        .mp(3, 3)
        .build()
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// `aura_membership_set` must return identical iteration order across 10 calls
/// on the same state (BTreeSet guarantee; guards against regression to HashSet).
#[test]
fn aura_membership_set_iteration_order_is_stable() {
    // Source at origin, two targets within radius 1.
    let src = uid(1);
    let tgt_a = uid(2);
    let tgt_b = uid(3);

    let mut src_unit = make_unit(1, Team::Enemy, Hex::ORIGIN);
    src_unit.auras = vec![AuraDef { radius: 1, status_id: sid("slow"), applies_to: TeamRelation::Enemies }];
    let units = vec![
        src_unit,
        make_unit(2, Team::Player, Hex::new(1, 0)),
        make_unit(3, Team::Player, Hex::new(0, 1)),
    ];

    let order = vec![src, tgt_a, tgt_b];
    let mut state = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(order, 0);

    let content = StubContent::new();

    // Collect 10 snapshots of the iteration order.
    let snapshots: Vec<Vec<(UnitId, UnitId, StatusId)>> = (0..10)
        .map(|_| state.aura_membership_set(&content).into_iter().collect())
        .collect();

    // All 10 must be identical.
    for (i, snap) in snapshots.iter().enumerate() {
        assert_eq!(
            &snapshots[0],
            snap,
            "snapshot {i} differs from snapshot 0 — iteration order not stable"
        );
    }

    // Sanity: the set is non-empty (aura does apply to both targets).
    assert!(!snapshots[0].is_empty(), "expected at least one aura membership triple");
}

/// BTreeSet ordering is lexicographic on (UnitId, UnitId, StatusId) triples —
/// verify that larger UIDs sort after smaller ones.
#[test]
fn aura_membership_set_sorted_by_unit_id() {
    let src = uid(10);   // source has a large id
    let tgt_a = uid(1);  // small target id
    let tgt_b = uid(5);  // medium target id

    let mut src_unit = make_unit(10, Team::Enemy, Hex::ORIGIN);
    src_unit.auras = vec![AuraDef { radius: 2, status_id: sid("aura"), applies_to: TeamRelation::Enemies }];
    let units = vec![
        src_unit,
        make_unit(1,  Team::Player, Hex::new(1, 0)),
        make_unit(5,  Team::Player, Hex::new(0, 1)),
    ];

    let mut state = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![src, tgt_a, tgt_b], 0);

    let content = StubContent::new();

    let triples: Vec<_> = state.aura_membership_set(&content).into_iter().collect();
    assert_eq!(triples.len(), 2);

    // BTreeSet on (UnitId(u64), ...) → sorted by target id ascending.
    assert!(triples[0].0 < triples[1].0, "triples must be sorted by target UnitId");
}
