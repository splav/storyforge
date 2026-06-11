//! Bench: engine `step(Action::Move)` vs legacy-path `sim::apply_step(Move)`.
//!
//! **Gate criterion 3:** engine ≤ legacy × 1.2.
//!
//! Scenario: 10-unit battle. Actor moves 4 hexes; 2 enemies are adjacent to
//! the path and fire AoOs. All dice use `ExpectedValue` (deterministic).
//!
//! "Legacy" here is `SimState::apply_step(PlanStep::Move)`, which internally
//! now calls the engine. The engine benchmark calls `step()` directly after
//! building a `CombatState`, skipping the snapshot⇄engine projection round-trip.
//! The ratio measures the overhead of the snapshot conversion layer.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use bevy::prelude::Entity;
use storyforge::combat::ai::plan::sim::SimState;
use storyforge::combat::ai::plan::types::PlanStep;
use storyforge::combat::ai::test_helpers::{ent, snapshot_from};
use storyforge::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use storyforge::combat::ai::world::tags::{AiTags, StatusTagCache};
use storyforge::combat::engine_bridge::entity_to_uid;
use storyforge::combat_engine::{
    action::Action,
    content::ContentView as EngineContentView,
    dice::ExpectedValue,
    state::{CombatState, RoundPhase},
    step::step,
};
use storyforge::content::abilities::CasterContext;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;

// ── Shared scenario setup ─────────────────────────────────────────────────────

fn make_unit_snap(id: u32, team: Team, col: i32, row: i32, aoo: Option<f32>) -> UnitSnapshot {
    UnitSnapshot {
        entity: ent(id),
        team,
        role: Default::default(),
        pos: hex_from_offset(col, row),
        hp: 30,
        max_hp: 30,
        armor: 2,
        armor_bonus: 0,
        magic_resist: 0,
        damage_taken_bonus: 0,
        action_points: 2,
        max_ap: 2,
        movement_points: 6,
        base_speed: 6,
        speed: 6,
        mana: None,
        rage: None,
        energy: None,
        abilities: Vec::new(),
        threat: 0.0,
        tags: AiTags::empty(),
        max_attack_range: 1,
        summoner: None,
        reactions_left: 1,
        aoo_expected_damage: aoo,
        statuses: Vec::new(),
        caster_ctx: CasterContext::default(),
        crit_fail_effect: Default::default(),
        damage_horizon: Vec::new(),
        ai_tuning_override: None,
        forced_mode: None,
    }
}

/// Build the 10-unit benchmark scenario.
///
/// - Actor (id=1, Player) at col=0 row=0, moves right to col=4.
/// - 2 enemies adjacent to steps in the path → fire AoOs (raw=5 each).
/// - 7 filler enemies far away (no AoO reach, no reactions impact).
fn build_scenario() -> (BattleSnapshot, Entity, Vec<storyforge::game::hex::Hex>) {
    let actor = make_unit_snap(1, Team::Player, 0, 0, None);
    let actor_id = actor.entity;

    // Enemy A: adjacent to (0,0), not adjacent to (1,0) → AoO on step 1.
    let enemy_a = make_unit_snap(2, Team::Enemy, -1, 0, Some(5.0));
    // Enemy B: adjacent to (1,0), not adjacent to (2,0) → AoO on step 2.
    // hex_from_offset(0,1) is a neighbor of (1,0) in even-r coordinates.
    let enemy_b = make_unit_snap(3, Team::Enemy, 0, 1, Some(5.0));

    let fillers: Vec<UnitSnapshot> = (4u32..=10)
        .map(|id| make_unit_snap(id, Team::Enemy, 10 + id as i32, 5, None))
        .collect();

    let mut units = vec![actor, enemy_a, enemy_b];
    units.extend(fillers);
    let snap = snapshot_from(units, 1);

    let path = vec![
        hex_from_offset(1, 0),
        hex_from_offset(2, 0),
        hex_from_offset(3, 0),
        hex_from_offset(4, 0),
    ];
    (snap, actor_id, path)
}

// ── ContentView adapter for engine bench ─────────────────────────────────────

/// Minimal ContentView stub for engine benchmarks.
///
/// AoO dice now live on Unit.caster_context.weapon_dice (5c.1); this struct
/// carries only the 4 static-content methods.
struct BenchContent;

impl BenchContent {
    fn from_snap(_snap: &BattleSnapshot) -> Self {
        Self
    }
}

impl EngineContentView for BenchContent {
    fn ability_def(&self, _: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
        None
    }
    fn status_def(&self, _: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> {
        None
    }
    fn unit_template(&self, _: &str) -> Option<combat_engine::UnitTemplate> {
        None
    }
}

fn snap_to_combat_state(snap: &BattleSnapshot) -> CombatState {
    CombatState::new(snap.state.units().to_vec(), 1, RoundPhase::ActorTurn, 0)
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

/// Measure `step()` directly (pure engine, no snapshot conversion).
fn bench_move_10units_engine(c: &mut Criterion) {
    let (snap, actor_id, path) = build_scenario();
    let content = BenchContent::from_snap(&snap);
    let actor_uid = entity_to_uid(actor_id);

    c.bench_function("bench_move_10units_engine", |b| {
        b.iter(|| {
            let mut state = snap_to_combat_state(&snap);
            let action = Action::Move {
                actor: actor_uid,
                path: path.clone(),
            };
            let _ = step(
                black_box(&mut state),
                black_box(action),
                &mut ExpectedValue,
                &content,
            );
            black_box(state);
        });
    });
}

/// Measure `sim.apply_step(PlanStep::Move)` — the full round-trip path
/// (snapshot → CombatState → step → project back).
fn bench_move_10units_legacy(c: &mut Criterion) {
    let (snap, actor_id, path) = build_scenario();
    let status_tags = StatusTagCache::default();
    let content = storyforge::content::content_view::ContentView::default();

    c.bench_function("bench_move_10units_legacy", |b| {
        b.iter(|| {
            let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
            let plan_step = PlanStep::Move { path: path.clone() };
            let outcome = sim.apply_step(
                black_box(&plan_step),
                &CasterContext::default(),
                &content,
                false,
            );
            black_box(outcome);
            black_box(sim);
        });
    });
}

criterion_group!(
    benches,
    bench_move_10units_engine,
    bench_move_10units_legacy
);
criterion_main!(benches);
