//! Bench: `combat_engine::step()` on a mid-encounter scenario.
//!
//! This is the "denominator" bench for `snapshot_rebuild.rs`.  Without knowing
//! how long a single `step()` call takes, the raw snapshot number has no context.
//! After running both benches we can answer: "snapshot rebuild is X% of step() time".
//!
//! Scenario: same 6-unit mid-encounter (2 player + 4 enemy) as `snapshot_rebuild.rs`.
//! Three representative actions are measured as sub-benches:
//!   1. `Move`    — actor walks 3 hexes (cheap, no reaction)
//!   2. `EndTurn` — trivial bookkeeping (AP reset + advance turn queue)
//!
//! Notes:
//! - Setup (snapshot → `CombatState`) is hoisted OUTSIDE `b.iter()`.
//! - `ExpectedValue` dice keeps results deterministic across criterion warmup
//!   iterations (no RNG variance in timing).
//! - Build WITHOUT `--features dev` — per CLAUDE.md bench conventions.
//!
//! See `docs/combat/perf-baseline.md` for captured numbers and verdict.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use storyforge::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
use storyforge::combat::ai::world::snapshot::BattleSnapshot;
use storyforge::combat::engine_bridge::entity_to_uid;
use storyforge::combat_engine::{
    action::Action,
    content::ContentView as EngineContentView,
    dice::ExpectedValue,
    state::{CombatState, RoundPhase},
    step::step,
};
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;
use bevy::prelude::Entity;

// ── ContentView stub ─────────────────────────────────────────────────────────

struct BenchContent;

impl EngineContentView for BenchContent {
    fn ability_def(&self, _: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<combat_engine::UnitTemplate> { None }
}

// ── Scenario construction ────────────────────────────────────────────────────

fn build_mid_encounter_snap() -> BattleSnapshot {
    let p1 = UnitBuilder::new(1, Team::Player, hex_from_offset(2, 3))
        .full_hp(35).ap(2).speed(6).threat(10.0).max_attack_range(1).build();
    let p2 = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 3))
        .max_hp(30).hp(18).ap(2).speed(5).threat(8.0).max_attack_range(3).build();
    let e1 = UnitBuilder::new(10, Team::Enemy, hex_from_offset(7, 3))
        .full_hp(25).ap(2).speed(4).threat(7.0).aoo(5.0, 1).build();
    let e2 = UnitBuilder::new(11, Team::Enemy, hex_from_offset(8, 4))
        .full_hp(25).ap(2).speed(4).threat(7.0).aoo(5.0, 1).build();
    let e3 = UnitBuilder::new(12, Team::Enemy, hex_from_offset(5, 2))
        .max_hp(20).hp(0).ap(0).speed(4).threat(0.0).build();
    let e4 = UnitBuilder::new(13, Team::Enemy, hex_from_offset(9, 5))
        .full_hp(30).ap(2).speed(6).threat(9.0).max_attack_range(3).build();
    snapshot_from(vec![p1, p2, e1, e2, e3, e4], 1)
}

fn snap_to_combat_state(snap: &BattleSnapshot) -> CombatState {
    CombatState::new(snap.state.units().to_vec(), 1, RoundPhase::ActorTurn, 0)
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

/// `step(Move)` — p2 (Player unit 2, at col=3 row=3) walks 3 hexes right.
/// Path stays clear of enemies at (7,3)/(8,4), so no AoO triggers.
fn bench_step_move(c: &mut Criterion) {
    let snap = build_mid_encounter_snap();
    let content = BenchContent;
    // p2: entity raw=2, at (3,3); move to (4,3),(5,3),(6,3)
    let actor_uid = entity_to_uid(Entity::from_raw_u32(2).expect("valid"));
    let path = vec![
        hex_from_offset(4, 3),
        hex_from_offset(5, 3),
        hex_from_offset(6, 3),
    ];

    c.bench_function("step_move_3hex", |b| {
        b.iter(|| {
            let mut state = snap_to_combat_state(&snap);
            let action = Action::Move { actor: actor_uid, path: path.clone() };
            let _ = step(black_box(&mut state), black_box(action), &mut ExpectedValue, &content);
            black_box(state);
        });
    });
}

/// `step(EndTurn)` — actor ends turn (AP reset + advance turn queue, trivial path).
fn bench_step_end_turn(c: &mut Criterion) {
    let snap = build_mid_encounter_snap();
    let content = BenchContent;
    let actor_entity = Entity::from_raw_u32(1).expect("valid entity");
    let actor_uid = entity_to_uid(actor_entity);

    c.bench_function("step_end_turn", |b| {
        b.iter(|| {
            let mut state = snap_to_combat_state(&snap);
            let action = Action::EndTurn { actor: actor_uid };
            let _ = step(black_box(&mut state), black_box(action), &mut ExpectedValue, &content);
            black_box(state);
        });
    });
}

criterion_group!(benches, bench_step_move, bench_step_end_turn);
criterion_main!(benches);
