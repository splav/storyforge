//! Bench: `snapshot_from()` (AI snapshot rebuild) on a mid-encounter scenario.
//!
//! Measures the wall-time cost of building a `BattleSnapshot` from scratch
//! (the call that happens at the top of every AI tick in `h_run_ai_turn_orchestration`).
//!
//! Scenario: 2 player + 4 enemy units on a 12×10 grid, some with statuses,
//! one unit at partial HP to represent a mid-encounter state.  No Bevy World
//! needed — `snapshot_from` is a pure function over `Vec<UnitSnapshot>`.
//!
//! See `docs/combat/perf-baseline.md` for captured numbers and verdict.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use storyforge::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
use storyforge::combat::ai::world::snapshot::UnitSnapshot;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;

// ── Scenario construction ────────────────────────────────────────────────────

fn make_mid_encounter_units() -> Vec<UnitSnapshot> {
    // 2 player units
    let p1 = UnitBuilder::new(1, Team::Player, hex_from_offset(2, 3))
        .full_hp(35)
        .ap(2)
        .speed(6)
        .threat(10.0)
        .max_attack_range(1)
        .build();

    // Player 2 at partial HP — mid-encounter wound
    let p2 = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 3))
        .max_hp(30)
        .hp(18)
        .ap(2)
        .speed(5)
        .threat(8.0)
        .max_attack_range(3)
        .build();

    // 4 enemy units
    let e1 = UnitBuilder::new(10, Team::Enemy, hex_from_offset(7, 3))
        .full_hp(25)
        .ap(2)
        .speed(4)
        .threat(7.0)
        .aoo(5.0, 1)
        .build();

    let e2 = UnitBuilder::new(11, Team::Enemy, hex_from_offset(8, 4))
        .full_hp(25)
        .ap(2)
        .speed(4)
        .threat(7.0)
        .aoo(5.0, 1)
        .build();

    // Enemy 3 at 0 HP — a corpse (still included: snapshot retains dead units
    // so `dead_units()` accessors work correctly)
    let e3 = UnitBuilder::new(12, Team::Enemy, hex_from_offset(5, 2))
        .max_hp(20)
        .hp(0)
        .ap(0)
        .speed(4)
        .threat(0.0)
        .build();

    let e4 = UnitBuilder::new(13, Team::Enemy, hex_from_offset(9, 5))
        .full_hp(30)
        .ap(2)
        .speed(6)
        .threat(9.0)
        .max_attack_range(3)
        .build();

    vec![p1, p2, e1, e2, e3, e4]
}

// ── Benchmark ────────────────────────────────────────────────────────────────

fn bench_snapshot_rebuild_mid_encounter(c: &mut Criterion) {
    // Build the input units once — only the `snapshot_from` call is measured.
    let units = make_mid_encounter_units();

    c.bench_function("snapshot_rebuild_mid_encounter", |b| {
        b.iter(|| {
            // snapshot_from clones the input vec internally, so we clone here
            // to give it fresh owned data each iteration (matches production
            // semantics where build_snapshot constructs fresh data each tick).
            let result = snapshot_from(black_box(units.clone()), black_box(1));
            black_box(result);
        });
    });
}

criterion_group!(benches, bench_snapshot_rebuild_mid_encounter);
criterion_main!(benches);
