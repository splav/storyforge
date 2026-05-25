//! Parity tests: engine `step(Action::Move)` vs legacy `sim::apply_move`.
//!
//! Each scenario runs both paths with identical inputs (deterministic
//! `ExpectedValue` dice) and asserts the final state fields agree.
//!
//! **Behaviour-change manifest (decision 6.3 / 6.5):**
//! Any scenario that legitimately differs between legacy and engine is
//! documented here with the reason; it is NOT a bug.

use bevy::prelude::Entity;
use storyforge::combat::ai::plan::sim::SimState;
use storyforge::combat::ai::plan::types::PlanStep;
use storyforge::combat::ai::test_helpers::{ent, snapshot_from_pairs, UnitBuilder};
use storyforge::combat::ai::world::snapshot::BattleSnapshot;
use storyforge::combat::ai::world::tags::StatusTagCache;

use storyforge::combat_engine::{
    action::Action,
    content::ContentView as EngineContentView,
    dice::ExpectedValue,
    state::{CombatState, RoundPhase},
    step::step,
};
use storyforge::combat::engine_bridge::entity_to_uid;
use storyforge::content::abilities::CasterContext;
use storyforge::combat_engine::StatusId;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Build a unit pair with parity-test defaults: hp/max_hp=30, ap=2, speed=6, threat=0.
/// Pass `aoo=Some(d)` to arm the unit with expected AoO damage `d` and `reactions` reactions.
fn make_unit(
    id: u32,
    team: Team,
    col: i32,
    row: i32,
    hp: i32,
    aoo: Option<f32>,
    reactions: i32,
) -> (storyforge::combat_engine::state::Unit, storyforge::combat::ai::world::cache::UnitAiCache) {
    let b = UnitBuilder::new(id, team, hex_from_offset(col, row))
        .hp(hp)
        .max_hp(30)
        .ap(2)
        .speed(6)
        .threat(0.0);
    let b = if let Some(d) = aoo { b.aoo(d, reactions) } else { b };
    b.build_pair()
}

/// Run the engine path on `snap` and return the final `CombatState`.
fn run_engine(snap: &BattleSnapshot, actor_id: Entity, path: Vec<storyforge::game::hex::Hex>) -> CombatState {
    let content = SnapContent::from_snap(snap);
    let actor_uid = entity_to_uid(actor_id);
    let action = Action::Move { actor: actor_uid, path };
    let mut state = CombatState::new(snap.state.units().to_vec(), 1, RoundPhase::ActorTurn, 0);
    // Ignore result: on TargetGone the state is rolled back, which is the
    // correct observable outcome to compare against.
    let _ = step(&mut state, action, &mut ExpectedValue, &content);
    state
}

/// Run the sim path on `snap` and return the final `BattleSnapshot`.
fn run_sim(snap: &BattleSnapshot, actor_id: Entity, path: Vec<storyforge::game::hex::Hex>) -> BattleSnapshot {
    let status_tags = StatusTagCache::default();
    let content = storyforge::content::content_view::ContentView::default();
    let mut sim = SimState::from_snapshot(snap, actor_id, &status_tags);
    sim.apply_step(
        &PlanStep::Move { path },
        &CasterContext::default(),
        &content,
        false,
    );
    sim.into_snapshot()
}

/// ContentView adapter for parity tests. After Phase 5c.1 the engine reads
/// AoO dice directly from `Unit.aoo_dice` (set by `init_state_from_ecs`),
/// so this stub no longer needs to materialize the dice map itself.
struct SnapContent;

impl SnapContent {
    fn from_snap(_snap: &BattleSnapshot) -> Self {
        Self
    }
}

impl EngineContentView for SnapContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId) -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&storyforge::combat_engine::StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

// ── Scenario 1: pure move, no enemies ────────────────────────────────────────

/// Pure move with no enemies: position and MP decremented identically.
#[test]
fn parity_pure_move_no_enemies() {
    let actor_id = ent(1);
    let snap = snapshot_from_pairs(vec![
        make_unit(1, Team::Player, 0, 0, 20, None, 0),
    ], 1);
    let path = vec![
        hex_from_offset(1, 0),
        hex_from_offset(2, 0),
        hex_from_offset(3, 0),
    ];

    let engine_state = run_engine(&snap, actor_id, path.clone());
    let sim_snap     = run_sim(&snap, actor_id, path);

    let actor_uid   = entity_to_uid(actor_id);
    let engine_unit = engine_state.unit(actor_uid).expect("actor in engine state");
    let sim_unit    = sim_snap.unit(actor_id).expect("actor in sim snapshot");

    assert_eq!(engine_unit.pos, hex_from_offset(3, 0), "engine: final pos");
    assert_eq!(sim_unit.pos,    hex_from_offset(3, 0), "sim: final pos");

    // MP: 6 - 3 = 3.
    let engine_mp = engine_unit.pools[storyforge::combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    let sim_mp    = sim_unit.pools[storyforge::combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    assert_eq!(engine_mp, 3, "engine: MP after 3-hex move");
    assert_eq!(sim_mp,    3, "sim: MP after 3-hex move");

    assert_eq!(engine_unit.hp, sim_unit.hp, "hp parity: no enemies → no damage");
}

// ── Scenario 2: move with no-disengage (enemy at start AND destination) ───────

/// Enemy is adjacent to both the start pos AND the destination — the actor
/// never leaves adjacency, so no AoO triggers.
///
/// Parity: both paths agree on no damage and no reaction decrement.
///
/// Positions are discovered at runtime via `all_neighbors()` so we don't have
/// to hard-code the even-r adjacency layout.
#[test]
fn parity_move_no_aoo_stays_adjacent() {
    use std::collections::HashSet;

    // Pick start, then dest as one of start's neighbors. Enemy goes to a
    // shared neighbor of (start, dest).
    let actor_start = hex_from_offset(2, 1);
    let dest        = actor_start.all_neighbors()[0];
    let dest_nbs: HashSet<_> = dest.all_neighbors().into_iter().collect();
    let enemy_pos = actor_start
        .all_neighbors()
        .into_iter()
        .find(|n| *n != dest && dest_nbs.contains(n))
        .expect("two adjacent hexes share at least one common neighbor");

    let [a_col, a_row] = storyforge::game::hex::hex_to_offset(actor_start);
    let [e_col, e_row] = storyforge::game::hex::hex_to_offset(enemy_pos);

    let actor_id = ent(1);
    let enemy_id = ent(2);
    let snap = snapshot_from_pairs(vec![
        make_unit(1, Team::Player, a_col, a_row, 20, None, 0),
        make_unit(2, Team::Enemy,  e_col, e_row, 20, Some(8.0), 1),
    ], 1);

    assert_eq!(actor_start.unsigned_distance_to(enemy_pos), 1,
        "enemy must be adjacent to start");
    assert_eq!(dest.unsigned_distance_to(enemy_pos), 1,
        "enemy must also be adjacent to destination — otherwise AoO fires");

    let path = vec![dest];

    let engine_state = run_engine(&snap, actor_id, path.clone());
    let sim_snap     = run_sim(&snap, actor_id, path);

    let actor_uid   = entity_to_uid(actor_id);
    let enemy_uid   = entity_to_uid(enemy_id);
    let engine_actor = engine_state.unit(actor_uid).unwrap();
    let sim_actor    = sim_snap.unit(actor_id).unwrap();

    // No damage — enemy never disengage-triggered.
    assert_eq!(engine_actor.hp, 20, "engine: actor hp unchanged (no AoO)");
    assert_eq!(sim_actor.hp,    20, "sim: actor hp unchanged (no AoO)");

    // Enemy reactions_left unchanged.
    assert_eq!(engine_state.unit(enemy_uid).unwrap().reactions_left, 1,
        "engine: enemy reaction not consumed");
    assert_eq!(sim_snap.unit(enemy_id).unwrap().reactions_left, 1,
        "sim: enemy reaction not consumed");
}

// ── Scenario 3: AoO chain — two enemies, per-target ordering ─────────────────

/// Mover passes adjacent to two enemies sequentially; each fires one AoO.
/// Both paths agree on final HP, positions, and reaction counts.
#[test]
fn parity_aoo_chain_two_enemies() {
    // Actor at (1,0), moves (2,0) → (4,0).
    // Enemy A at (0,0): adjacent to (1,0), not (2,0) → AoO on first step.
    // Enemy B at (3,1): adjacent to (3,0), not (4,0) → AoO on third step.
    // raw AoO = 4 for both; actor armor = 0 → final = 4 each.
    // Actor starts at hp=20, takes 4+4=8 damage → hp=12.
    let actor_id   = ent(1);
    let enemy_a_id = ent(2);
    let enemy_b_id = ent(3);

    let snap = snapshot_from_pairs(vec![
        UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0))
            .hp(20).max_hp(30).ap(2).speed(6).threat(0.0)
            .movement_points(5)
            .build_pair(),
        make_unit(2, Team::Enemy, 0, 0, 20, Some(4.0), 1),
        make_unit(3, Team::Enemy, 3, 1, 20, Some(4.0), 1),
    ], 1);

    // Verify adjacency for enemy A (should disengage on step to (2,0)).
    let start     = hex_from_offset(1, 0);
    let step1     = hex_from_offset(2, 0);
    let ea_pos    = hex_from_offset(0, 0);
    assert_eq!(start.unsigned_distance_to(ea_pos), 1, "enemy A adjacent to start");
    assert_ne!(step1.unsigned_distance_to(ea_pos), 1, "enemy A not adjacent after step 1");

    // Verify adjacency for enemy B (should disengage on step to (4,0)).
    let step3     = hex_from_offset(4, 0);
    let step2     = hex_from_offset(3, 0);
    let eb_pos    = hex_from_offset(3, 1);
    assert_eq!(step2.unsigned_distance_to(eb_pos), 1, "enemy B adjacent to (3,0)");
    assert_ne!(step3.unsigned_distance_to(eb_pos), 1, "enemy B not adjacent after step to (4,0)");

    let path = vec![
        hex_from_offset(2, 0),
        hex_from_offset(3, 0),
        hex_from_offset(4, 0),
    ];

    let engine_state = run_engine(&snap, actor_id, path.clone());
    let sim_snap     = run_sim(&snap, actor_id, path);

    let actor_uid   = entity_to_uid(actor_id);
    let ea_uid      = entity_to_uid(enemy_a_id);
    let eb_uid      = entity_to_uid(enemy_b_id);

    let engine_actor = engine_state.unit(actor_uid).expect("actor in engine");
    let sim_actor    = sim_snap.unit(actor_id).expect("actor in sim");

    assert_eq!(engine_actor.hp, sim_actor.hp,
        "actor hp must match: each path took 4+4=8 damage from two AoOs, hp 20→12");
    assert_eq!(engine_actor.pos, sim_actor.pos, "actor final position must match");

    let engine_mp = engine_actor.pools[storyforge::combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    let sim_mp    = sim_actor.pools[storyforge::combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    assert_eq!(engine_mp, sim_mp, "movement_points must match");

    // Both enemies consumed their reactions.
    assert_eq!(engine_state.unit(ea_uid).unwrap().reactions_left, 0,
        "engine: enemy A reaction consumed");
    assert_eq!(sim_snap.unit(enemy_a_id).unwrap().reactions_left, 0,
        "sim: enemy A reaction consumed");
    assert_eq!(engine_state.unit(eb_uid).unwrap().reactions_left, 0,
        "engine: enemy B reaction consumed");
    assert_eq!(sim_snap.unit(enemy_b_id).unwrap().reactions_left, 0,
        "sim: enemy B reaction consumed");
}

// ── Scenario 4: AoO kills mover mid-path — truncation parity ─────────────────

/// Lethal-AoO with two flanking enemies on a single-step path.
///
/// Under actor-liveness truncation the first AoO kills the mover and the second
/// is never fired:
/// - Engine: returns `Ok`, mover dead at destination, second enemy reactions_left=1.
/// - Sim: same outcome (sim routes through engine's step()).
///
/// Both paths agree: mover dead, mover at destination, one enemy reaction spent.
#[test]
fn parity_aoo_kills_mover_mid_path_truncates() {
    // Actor has 1 hp. Two enemies both adjacent to start; both AoO on step 1.
    // First AoO (raw=5) kills the mover; second is skipped by liveness check.

    let start = hex_from_offset(0, 0);
    let step1 = hex_from_offset(3, 0);  // big jump so all neighbors disengage

    // Two enemies adjacent to start.
    let neighbors = start.all_neighbors();
    let ea_pos = neighbors[0];
    let eb_pos = neighbors[1];

    assert_eq!(start.unsigned_distance_to(ea_pos), 1);
    assert_eq!(start.unsigned_distance_to(eb_pos), 1);
    assert_ne!(step1.unsigned_distance_to(ea_pos), 1,
        "enemy A must not be adjacent to destination (would prevent AoO)");
    assert_ne!(step1.unsigned_distance_to(eb_pos), 1,
        "enemy B must not be adjacent to destination (would prevent AoO)");

    let actor_id   = ent(1);
    let enemy_a_id = ent(2);
    let enemy_b_id = ent(3);

    let snap = snapshot_from_pairs(vec![
        UnitBuilder::new(1, Team::Player, start)
            .hp(1).max_hp(30).ap(2).speed(6).threat(0.0)
            .movement_points(10)
            .build_pair(),
        UnitBuilder::new(2, Team::Enemy, ea_pos)
            .full_hp(20).ap(0).speed(3).movement_points(0).threat(0.0)
            .aoo(5.0, 1)
            .build_pair(),
        UnitBuilder::new(3, Team::Enemy, eb_pos)
            .full_hp(20).ap(0).speed(3).movement_points(0).threat(0.0)
            .aoo(5.0, 1)
            .build_pair(),
    ], 1);

    let path = vec![step1];

    // ── Engine path ───────────────────────────────────────────────────────────
    let content = SnapContent::from_snap(&snap);
    let actor_uid = entity_to_uid(actor_id);
    let ea_uid    = entity_to_uid(enemy_a_id);
    let eb_uid    = entity_to_uid(enemy_b_id);
    let action = Action::Move { actor: actor_uid, path: path.clone() };
    let mut engine_state = CombatState::new(snap.state.units().to_vec(), 1, RoundPhase::ActorTurn, 0);
    let engine_result = step(&mut engine_state, action, &mut ExpectedValue, &content);

    // ── Sim path ──────────────────────────────────────────────────────────────
    let sim_snap = run_sim(&snap, actor_id, path);

    // ── Assert truncation parity ──────────────────────────────────────────────
    // Engine must succeed.
    assert!(engine_result.is_ok(), "engine should return Ok under truncation semantics");

    let engine_actor = engine_state.unit(actor_uid).unwrap();
    assert_eq!(engine_actor.hp, 0, "engine: mover dead after lethal AoO");
    assert_eq!(engine_actor.pos, step1, "engine: mover at step1 (hit position)");

    // Exactly one enemy spent its reaction; the other did not.
    let ea_reactions = engine_state.unit(ea_uid).unwrap().reactions_left;
    let eb_reactions = engine_state.unit(eb_uid).unwrap().reactions_left;
    assert_eq!(ea_reactions + eb_reactions, 1,
        "engine: one enemy fired (reactions=0), other did not (reactions=1)");

    // Sim parity: mover position and hp match engine.
    let sim_actor = sim_snap.unit(actor_id).expect("actor present in sim snapshot");
    assert_eq!(sim_actor.hp, engine_actor.hp,
        "sim: actor hp must match engine");
    assert_eq!(sim_actor.pos, engine_actor.pos,
        "sim: actor position must match engine");
}
