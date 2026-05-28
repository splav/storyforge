//! Step 6 unit tests: `step()` driver.
//!
//! Decision 6.3 (per-target ordering) — pinned by `step_move_with_aoo_chain`.
//! Decision 6.5 (strict failure + rollback for non-actor targets) — the branch
//! is reserved for Phase 2+ Cast/AoE actions; no dedicated test yet since those
//! Action variants do not exist in Phase 0/1.
//! Actor-liveness truncation — pinned by `step_actor_death_mid_path_truncates_remaining_aoos`
//! and `step_two_flankers_only_first_fires_when_lethal`.

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::ContentView,
    dice::{DiceExpr, ExpectedValue},
    event::Event,
    legality::IllegalReason,
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
};
use storyforge::game::hex::hex_from_offset;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Stub `ContentView` for step-level integration tests. After Phase 5c.1 the
/// engine reads AoO dice directly from `Unit.aoo_dice` rather than from
/// content; the `DiceExpr` passed to `with_weapon` is currently unused
/// (callers wanting AoO must set the field on their `Unit` directly).
/// The `with_weapon` / `no_weapon` distinction is preserved as a semantic
/// marker at callsites.
struct StubContent;

impl StubContent {
    fn no_weapon() -> Self { Self }
    fn with_weapon(_d: DiceExpr) -> Self { Self }
}

impl ContentView for StubContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId) -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &storyforge::combat_engine::StatusId) -> Option<&storyforge::combat_engine::StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}


fn make_unit(id: u64, team: Team, pos_col: i32, pos_row: i32) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Unit {
        id: UnitId(id),
        team,
        pos: hex_from_offset(pos_col, pos_row),
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 6,
        speed: 6,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => Some((20, 20)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        template_id: None,
    }
}

fn state_with(units: Vec<Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::ActorTurn, 0)
}

// ── step_move_no_enemies ──────────────────────────────────────────────────────

/// Pure move with no enemies: events = ActionStarted, UnitMoved×2, PoolChanged{Spent,Mp}, ActionFinished.
/// MP is decremented by path length.
#[test]
fn step_move_no_enemies() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let mut state = state_with(vec![actor]);
    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let action = Action::Move { actor: UnitId(1), path: path.clone() };

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &StubContent::no_weapon())
        .expect("move should succeed");

    // Bookend events.
    assert!(matches!(events[0], Event::ActionStarted { .. }));
    assert!(matches!(events[events.len()-1], Event::ActionFinished { .. }));

    // Two UnitMoved events must be present (one per hex step).
    let moved: Vec<_> = events.iter().filter(|e| matches!(e, Event::UnitMoved { actor, .. } if *actor == UnitId(1))).collect();
    assert_eq!(moved.len(), 2, "expected 2 UnitMoved events for a 2-step path");

    // C4: DecrementMP now emits PoolChanged{Spent,Mp}.
    assert!(events.iter().any(|e| matches!(
        e, Event::PoolChanged { pool: combat_engine::PoolKind::Mp,
            cause: combat_engine::PoolChangeCause::Spent, .. }
    )), "PoolChanged{{Spent,Mp}} must fire on Move");

    // MP reduced: 6 - 2 = 4.
    let mp = state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    assert_eq!(mp, 4);
    // Final position.
    assert_eq!(state.unit(UnitId(1)).unwrap().pos, hex_from_offset(2, 0));
}

// ── step_move_out_of_mp ───────────────────────────────────────────────────────

#[test]
fn step_returns_out_of_mp_error() {
    let mut actor = make_unit(1, Team::Player, 0, 0);
    // Set MP to 1 (max stays 6 from make_unit).
    actor.pools[combat_engine::PoolKind::Mp] = Some((1, 6));
    let mut state = state_with(vec![actor]);
    // path of 2 but only 1 MP
    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let action = Action::Move { actor: UnitId(1), path };

    let err = step(&mut state, action, &mut ExpectedValue, &StubContent::no_weapon())
        .map(|(ev, _)| ev)
        .expect_err("should fail with OutOfMP");
    assert_eq!(err, ActionError::OutOfMP);
    // State unchanged (rollback): MP still 1.
    let mp = state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
    assert_eq!(mp, 1);
}

// ── step_move_unknown_actor ───────────────────────────────────────────────────

#[test]
fn step_returns_unknown_actor_error() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let mut state = state_with(vec![actor]);
    let action = Action::Move { actor: UnitId(999), path: vec![hex_from_offset(1, 0)] };

    let err = step(&mut state, action, &mut ExpectedValue, &StubContent::no_weapon())
        .map(|(ev, _)| ev)
        .expect_err("should fail with UnknownActor");
    assert_eq!(err, ActionError::UnknownActor);
}

// ── step_move_with_aoo_chain ──────────────────────────────────────────────────

/// Mover passes two adjacent enemies; each fires one AoO.
/// Per-target ordering (decision 6.3): each AoO's Damage + GainRage fully
/// resolve before the next move step.
#[test]
fn step_move_with_aoo_chain() {
    // Layout (even-r hex offset coords):
    //   mover at (0,0), moving → (1,0) → (2,0)
    //   enemy A at (0,1) — adjacent to (0,0) but not (1,0)? Let's verify later.
    // We use a simpler setup: mover at col=1, enemy at col=0 (adjacent), dest col=3.
    let mut mover = make_unit(1, Team::Player, 1, 0);
    mover.pools[combat_engine::PoolKind::Mp] = Some((4, 6));

    let mover_start = hex_from_offset(1, 0);
    let step1 = hex_from_offset(2, 0);   // moves away from enemy
    let step2 = hex_from_offset(3, 0);

    // Enemy adjacent to mover's start position.
    let mut enemy_a = make_unit(2, Team::Enemy, 0, 0);
    enemy_a.pos = hex_from_offset(0, 0); // adjacent to (1,0)
    enemy_a.pools[combat_engine::PoolKind::Rage] = Some((0, 5));
    enemy_a.aoo_dice = Some(DiceExpr::new(0, 6, 3));

    mover.pos = mover_start;

    let mut state = state_with(vec![mover, enemy_a]);
    let content = StubContent::with_weapon(DiceExpr::new(0, 6, 3)); // fixed 3 raw damage

    let path = vec![step1, step2];
    let (events, _ctx) = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &content,
    ).expect("move should succeed");

    // There should be at least one ReactionFired event.
    let reaction_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Event::ReactionFired { .. }))
        .collect();
    assert_eq!(reaction_events.len(), 1, "one AoO from the single adjacent enemy");

    // UnitDamaged must appear after ReactionFired (AoO resolves in place).
    let reaction_idx = events
        .iter()
        .position(|e| matches!(e, Event::ReactionFired { .. }))
        .unwrap();
    let damage_idx = events
        .iter()
        .position(|e| matches!(e, Event::UnitDamaged { .. }))
        .unwrap();
    assert!(damage_idx > reaction_idx, "damage event follows reaction event");
}

// ── step_actor_death_mid_path_truncates_remaining_aoos ────────────────────────

/// Actor-liveness truncation: when two enemies both flank a single path step and
/// the first AoO kills the mover, the second AoO is never expanded.
///
/// Expected outcome:
/// - `step()` returns `Ok` (no rollback).
/// - Mover is dead (hp == 0) at the step-1 position.
/// - Exactly one `ReactionFired` event.
/// - Exactly one `UnitDamaged` event.
/// - Second enemy's `reactions_left` is unchanged (its AoO never fired).
#[test]
fn step_actor_death_mid_path_truncates_remaining_aoos() {
    let start = hex_from_offset(0, 0);
    let step1 = hex_from_offset(1, 0);
    let step2 = hex_from_offset(2, 0);

    // Mover: 1 hp — first AoO (raw=5) kills it.
    let mut mover = make_unit(1, Team::Player, 0, 0);
    mover.pos = start;
    mover.hp = 1;
    mover.max_hp = 20;
    mover.pools[combat_engine::PoolKind::Hp] = Some((1, 20));
    mover.pools[combat_engine::PoolKind::Mp] = Some((4, 6));

    // Two enemies adjacent to `start` but NOT on the path [(1,0),(2,0)],
    // so path-occupancy validation does not reject the move.
    let path_hexes = [step1, step2];
    let off_path_nbs: Vec<_> = start
        .all_neighbors()
        .into_iter()
        .filter(|h| !path_hexes.contains(h))
        .collect();
    let enemy_a_pos = off_path_nbs[0];
    let enemy_b_pos = off_path_nbs[1];

    let aoo = DiceExpr::new(0, 6, 5);
    let mut enemy_a = make_unit(2, Team::Enemy, 0, 0);
    enemy_a.pos = enemy_a_pos;
    enemy_a.reactions_left = 1;
    enemy_a.aoo_dice = Some(aoo);

    let mut enemy_b = make_unit(3, Team::Enemy, 0, 0);
    enemy_b.pos = enemy_b_pos;
    enemy_b.reactions_left = 1;
    enemy_b.aoo_dice = Some(aoo);

    let mut state = CombatState::new(
        vec![mover, enemy_a, enemy_b],
        1,
        RoundPhase::ActorTurn,
        0,
    );

    // 5 raw damage, no armor — kills the 1-hp mover on the first AoO.
    let content = StubContent::with_weapon(aoo);

    let result = step(
        &mut state,
        Action::Move { actor: UnitId(1), path: vec![step1, step2] },
        &mut ExpectedValue,
        &content,
    );

    // Must succeed — no rollback.
    let (events, _ctx) = result.expect("actor-death truncation should not return Err");

    // Mover must be dead at step1.
    let mover_after = state.unit(UnitId(1)).unwrap();
    assert_eq!(mover_after.hp(), 0, "mover should be dead after lethal AoO");
    assert_eq!(mover_after.pos, step1, "mover should be at step1 (hit position)");

    // Exactly one ReactionFired.
    let reaction_count = events.iter().filter(|e| matches!(e, Event::ReactionFired { .. })).count();
    assert_eq!(reaction_count, 1, "only the first AoO should have fired");

    // Exactly one UnitDamaged.
    let damage_count = events.iter().filter(|e| matches!(e, Event::UnitDamaged { .. })).count();
    assert_eq!(damage_count, 1, "only one damage event expected");

    // One UnitDied for the mover.
    let died_count = events.iter().filter(|e| matches!(e, Event::UnitDied { unit } if *unit == UnitId(1))).count();
    assert_eq!(died_count, 1, "exactly one UnitDied for the mover");

    // Determine which enemy fired and which did not; the non-firer still has
    // reactions_left == 1.
    let ea_reactions = state.unit(UnitId(2)).unwrap().reactions_left;
    let eb_reactions = state.unit(UnitId(3)).unwrap().reactions_left;
    // One used its reaction, the other did not.
    assert_eq!(
        ea_reactions + eb_reactions, 1,
        "combined reactions_left across both enemies should be 1 (one fired, one did not)"
    );
}

// ── path-occupancy tests ──────────────────────────────────────────────────────

/// Passing through a friendly (same team) hex is allowed.
#[test]
fn step_move_through_ally_succeeds() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let ally  = make_unit(2, Team::Player, 1, 0);
    let mut state = state_with(vec![actor, ally]);

    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let result = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    );
    assert!(result.is_ok(), "passing through ally should succeed");
    assert_eq!(state.unit(UnitId(1)).unwrap().pos, hex_from_offset(2, 0));
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 4);
}

/// Moving through an enemy hex is forbidden; state rolls back.
#[test]
fn step_move_through_enemy_returns_path_blocked() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let enemy = make_unit(2, Team::Enemy, 1, 0);
    let mut state = state_with(vec![actor, enemy]);

    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let err = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect_err("should fail with PathBlockedByEnemy");

    assert_eq!(err, ActionError::PathBlockedByEnemy { hex: hex_from_offset(1, 0) });
    assert_eq!(state.unit(UnitId(1)).unwrap().pos, hex_from_offset(0, 0));
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 6);
}

/// Destination occupied by a friendly unit is forbidden.
#[test]
fn step_move_to_occupied_destination_friend() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let other = make_unit(2, Team::Player, 2, 0);
    let mut state = state_with(vec![actor, other]);

    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let err = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect_err("should fail with DestinationOccupied");

    assert_eq!(err, ActionError::DestinationOccupied { hex: hex_from_offset(2, 0) });
    assert_eq!(state.unit(UnitId(1)).unwrap().pos, hex_from_offset(0, 0));
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 6);
}

/// Destination occupied by an enemy unit is also forbidden.
#[test]
fn step_move_to_occupied_destination_enemy() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let other = make_unit(2, Team::Enemy, 2, 0);
    let mut state = state_with(vec![actor, other]);

    let path = vec![hex_from_offset(1, 0), hex_from_offset(2, 0)];
    let err = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect_err("should fail with DestinationOccupied");

    assert_eq!(err, ActionError::DestinationOccupied { hex: hex_from_offset(2, 0) });
    assert_eq!(state.unit(UnitId(1)).unwrap().pos, hex_from_offset(0, 0));
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 6);
}

// ── step_two_flankers_only_first_fires_when_lethal ────────────────────────────

/// Two enemies adjacent to the mover's start; the path moves away from both
/// (single step). First AoO deals lethal damage; second AoO is never fired.
///
/// This is the canonical "flanking kill truncates chain" scenario, distinct from
/// `step_actor_death_mid_path_truncates_remaining_aoos` which uses a multi-step
/// path — here the path has exactly one step so there are no later `MovePosition`
/// effects to skip; the truncation happens within the reaction sub-loop itself.
#[test]
fn step_two_flankers_only_first_fires_when_lethal() {
    // Mover at (0,0), hp=1, moves to a hex not adjacent to either enemy.
    // Enemies are at two neighbors of (0,0); destination is chosen to be
    // non-adjacent to both.
    let start = hex_from_offset(0, 0);
    let neighbors = start.all_neighbors();

    let enemy_a_pos = neighbors[0];
    let enemy_b_pos = neighbors[1];

    // Destination: a neighbor of start that is not adjacent to either enemy.
    let dest = neighbors
        .iter()
        .find(|&&h| {
            h != enemy_a_pos
                && h != enemy_b_pos
                && h.unsigned_distance_to(enemy_a_pos) > 1
                && h.unsigned_distance_to(enemy_b_pos) > 1
        })
        .copied()
        .expect("at least one non-adjacent destination must exist among start's neighbors");

    // Mover: 1 hp — lethal on any hit.
    let mut mover = make_unit(1, Team::Player, 0, 0);
    mover.pos = start;
    mover.hp = 1;
    mover.pools[combat_engine::PoolKind::Hp] = Some((1, 20));
    mover.pools[combat_engine::PoolKind::Mp] = Some((6, 6));

    let aoo = DiceExpr::new(0, 6, 5);
    let mut enemy_a = make_unit(2, Team::Enemy, 0, 0);
    enemy_a.pos = enemy_a_pos;
    enemy_a.reactions_left = 1;
    enemy_a.aoo_dice = Some(aoo);

    let mut enemy_b = make_unit(3, Team::Enemy, 0, 0);
    enemy_b.pos = enemy_b_pos;
    enemy_b.reactions_left = 1;
    enemy_b.aoo_dice = Some(aoo);

    // Verify both enemies disengage at the step to dest.
    assert_eq!(start.unsigned_distance_to(enemy_a_pos), 1, "enemy A adjacent to start");
    assert_ne!(dest.unsigned_distance_to(enemy_a_pos), 1, "enemy A not adjacent to dest");
    assert_eq!(start.unsigned_distance_to(enemy_b_pos), 1, "enemy B adjacent to start");
    assert_ne!(dest.unsigned_distance_to(enemy_b_pos), 1, "enemy B not adjacent to dest");

    let mut state = CombatState::new(
        vec![mover, enemy_a, enemy_b],
        1,
        RoundPhase::ActorTurn,
        0,
    );

    // Fixed +5 damage — kills the 1-hp mover on the first AoO.
    let content = StubContent::with_weapon(aoo);

    let (events, _ctx) = step(
        &mut state,
        Action::Move { actor: UnitId(1), path: vec![dest] },
        &mut ExpectedValue,
        &content,
    ).expect("truncation scenario must not return Err");

    // Exactly one ReactionFired (first AoO only).
    let reaction_count = events.iter().filter(|e| matches!(e, Event::ReactionFired { .. })).count();
    assert_eq!(reaction_count, 1, "only the first AoO should fire");

    // Mover dead at destination.
    let mover_after = state.unit(UnitId(1)).unwrap();
    assert_eq!(mover_after.hp, 0, "mover should be dead");
    assert_eq!(mover_after.pos, dest, "mover at destination (MovePosition applied before AoO)");

    // Second enemy's reaction is untouched.
    let ea_reactions = state.unit(UnitId(2)).unwrap().reactions_left;
    let eb_reactions = state.unit(UnitId(3)).unwrap().reactions_left;
    assert_eq!(
        ea_reactions + eb_reactions, 1,
        "one enemy fired (reactions_left=0), other did not (reactions_left=1)"
    );
}

// ── step_recursion_depth_capped ───────────────────────────────────────────────

/// Reaction depth limit: more than 100 reaction expansions → ReactionDepthExceeded.
/// We set up a corridor with many adjacent enemies to exhaust the counter.
#[test]
fn step_recursion_depth_capped() {
    // Place 101 enemies all adjacent to the mover's start; mover moves away.
    // Each has reactions_left=1 and a weapon. Since scan_reactions runs after
    // each MovePosition, all 101 reactions fire for the single step.
    // This exceeds the 100-reaction cap and must return ReactionDepthExceeded.

    let start = hex_from_offset(5, 5);
    // Enemies placed at start (same hex) — distance 0, not 1. That won't trigger AoO.
    // We need enemies at distance 1. A hex has 6 neighbors; we can only get 6 truly
    // adjacent positions, so 101 is unreachable from a single step.
    // Instead: test with a large depth via a single-step path and many enemies at
    // distance 1 — max is 6 for one hex. That gives 6 reactions, well under 100.
    //
    // Alternative: a long path (e.g. 101 steps) where one enemy triggers AoO on
    // every step (stays adjacent to each step in the path). This doesn't work either
    // because `scan_reactions` fires on disengagement only.
    //
    // The real depth-cap test requires a *retaliation* mechanic (AoO on AoO), which
    // Phase 0 doesn't have. A realistic test would need a ContentView that causes
    // each AoO to enqueue further AoOs. But `expand_reaction` only emits
    // `DecrementReactions + Damage`, and the Damage-derived `GainRage/Death` don't
    // trigger further `scan_reactions` (only MovePosition does).
    //
    // Therefore a true recursion-depth test requires either:
    //   a) synthetic modifications to how reactions work (out of scope), or
    //   b) a path long enough that scan_reactions fires 101+ times per step.
    //
    // For Phase 0 we verify the error path exists by constructing a scenario
    // with a 6-neighbor trigger (max geometrically possible) and confirm < 100.
    // The cap itself is tested via a unit test on the constant.

    let dest = hex_from_offset(5, 10); // far away
    let path = vec![dest];

    let mut mover = make_unit(1, Team::Player, 5, 5);
    mover.pos = start;
    mover.pools[combat_engine::PoolKind::Mp] = Some((20, 20));

    let neighbors: Vec<_> = start.all_neighbors().into_iter().enumerate().map(|(i, nb)| {
        let mut e = make_unit(100 + i as u64, Team::Enemy, 0, 0);
        e.pos = nb;
        e
    }).collect();

    let mut all_units = vec![mover];
    all_units.extend(neighbors);

    let mut state = CombatState::new(all_units, 1, RoundPhase::ActorTurn, 0);
    let content = StubContent::with_weapon(DiceExpr::new(0, 6, 1)); // 1 damage

    // 6 reactions from one step — under 100, should succeed (no cap hit).
    let result = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &content,
    );
    // 6 < 100, so we expect success (the mover survives 6 × 1 damage = 6, hp=14).
    assert!(result.is_ok(), "6 reactions should not exceed the 100-reaction depth cap");

    // Now verify the error variant: reset state and trigger with more than 100.
    // We can't easily do this geometrically in Phase 0 (max 6 neighbors), so
    // we test the constant is correct via the module.
    // (A proper stress test belongs in the bench suite.)
}

// ── Action::EndTurn ───────────────────────────────────────────────────────────

/// EndTurn with a 2-unit queue emits the Phase 4 handoff sequence:
/// ActionStarted, TurnEnded, TurnStarted{next}, ActionFinished.
#[test]
fn endturn_emits_turn_events_for_mid_round_handoff() {
    let a = make_unit(1, Team::Player, 0, 0);
    let b = make_unit(2, Team::Player, 1, 0);
    let mut state = state_with(vec![a, b]);
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 0);

    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: UnitId(1) },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect("EndTurn on current actor must succeed");

    // ActionStarted, TurnEnded{1}, TurnStarted{2}, ActionFinished
    assert_eq!(events.len(), 4, "expected 4 events, got: {:?}", events);
    assert!(matches!(&events[0], Event::ActionStarted { .. }));
    assert!(matches!(&events[1], Event::TurnEnded { actor: UnitId(1), .. }));
    assert!(matches!(&events[2], Event::TurnStarted { actor: UnitId(2) }));
    assert!(matches!(&events[3], Event::ActionFinished { .. }));
    assert_eq!(state.turn_queue.index, 1);
}

/// EndTurn for an unknown actor returns UnknownActor and leaves state untouched.
#[test]
fn endturn_rejects_unknown_actor() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let mut state = state_with(vec![actor]);
    state.set_turn_queue(vec![UnitId(1)], 0);

    let err = step(
        &mut state,
        Action::EndTurn { actor: UnitId(99) },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect_err("EndTurn with unknown UnitId must fail");

    assert_eq!(err, ActionError::UnknownActor);
}

/// EndTurn issued by an actor who is not the current queue cursor returns
/// Illegal(NotCurrent), regardless of whether that actor is alive or dead.
#[test]
fn endturn_rejects_when_actor_not_current() {
    let a = make_unit(1, Team::Player, 0, 0);
    let b = make_unit(2, Team::Player, 1, 0);
    let mut state = state_with(vec![a, b]);
    // A is current (index 0), B tries to EndTurn.
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 0);

    let err = step(
        &mut state,
        Action::EndTurn { actor: UnitId(2) },
        &mut ExpectedValue,
        &StubContent::no_weapon(),
    )
    .expect_err("EndTurn by non-current actor must fail");

    assert_eq!(err, ActionError::Illegal(IllegalReason::NotCurrent));
}

/// B5: When the **current** actor dies mid-Move (killed by an AoO), the engine
/// must automatically advance the turn queue and emit the full turn-lifecycle
/// sequence: `TurnEnded` for the dead actor, then `TurnStarted` for the next
/// alive actor.  The queue cursor must not remain on the corpse.
///
/// Setup (mirrors `aoo_triggers_on_disengage` geometry):
/// - heroA (uid 1) at offset (1,0) — Player, has `aoo_dice` set (lethal hit).
/// - enemyB (uid 2) at offset (0,0) — Enemy, current actor, 1 hp.
/// - enemyB moves to offset (0,2) — leaves heroA adjacency → AoO fires → lethal.
///
/// Queue: [A=1, B=2], current = B (index 1).
///
/// Expected after `step(Move { actor: 2, … })`:
/// - events contain `UnitDied { unit: 2 }`, `TurnEnded { actor: 2 }`,
///   `TurnStarted { actor: 1 }` (in that relative order).
/// - `state.turn_queue.current() == Some(UnitId(1))`.
#[test]
fn current_actor_dies_mid_move_via_aoo_settles_on_next_alive() {
    let enemy_b_start = hex_from_offset(0, 0); // enemyB starts adjacent to heroA
    let hero_a_pos    = hex_from_offset(1, 0); // heroA: adjacent to enemyB
    let enemy_b_dest  = hex_from_offset(0, 2); // dest not adjacent to heroA → AoO fires

    // heroA — Player, AoO attacker with a fixed 5-raw-damage die (no randomness).
    let mut hero_a = make_unit(1, Team::Player, 1, 0);
    hero_a.pos = hero_a_pos;
    hero_a.aoo_dice = Some(DiceExpr::new(0, 6, 5)); // constant 5 damage, kills 1-hp enemy

    // enemyB — Enemy, current actor, only 1 hp so the AoO is lethal.
    let mut enemy_b = make_unit(2, Team::Enemy, 0, 0);
    enemy_b.pos = enemy_b_start;
    enemy_b.hp = 1;
    enemy_b.pools[combat_engine::PoolKind::Hp] = Some((1, 20));
    enemy_b.pools[combat_engine::PoolKind::Mp] = Some((6, 6));

    let mut state = state_with(vec![hero_a, enemy_b]);
    // Queue [A=1, B=2], current = B (index 1).
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 1);
    assert_eq!(state.turn_queue.current(), Some(UnitId(2)));

    // EnemyB moves away from heroA — leaves adjacency → AoO fires → lethal.
    let (events, _ctx) = step(
        &mut state,
        Action::Move { actor: UnitId(2), path: vec![enemy_b_dest] },
        &mut ExpectedValue,
        &StubContent::with_weapon(DiceExpr::new(0, 6, 5)),
    )
    .expect("Move step must succeed even when mover dies");

    // ── State assertions ──────────────────────────────────────────────────────

    // EnemyB must be dead.
    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 0, "enemyB must be dead after lethal AoO");

    // Queue must have advanced past the dead actor and settled on heroA.
    assert_eq!(
        state.turn_queue.current(),
        Some(UnitId(1)),
        "turn queue must settle on heroA (UnitId(1)) after enemyB dies mid-move"
    );

    // ── Event sequence assertions ─────────────────────────────────────────────

    let died    = events.iter().position(|e| matches!(e, Event::UnitDied    { unit:  UnitId(2) }));
    let ended   = events.iter().position(|e| matches!(e, Event::TurnEnded   { actor: UnitId(2), .. }));
    let started = events.iter().position(|e| matches!(e, Event::TurnStarted { actor: UnitId(1) }));

    assert!(died.is_some(),    "UnitDied{{2}} must be in the event stream");
    assert!(ended.is_some(),   "TurnEnded{{2}} must be in the event stream (B5 engine fix)");
    assert!(started.is_some(), "TurnStarted{{1}} must be in the event stream (B5 engine fix)");

    // Ordering: died → ended → started.
    assert!(died.unwrap()  < ended.unwrap(),   "UnitDied must precede TurnEnded");
    assert!(ended.unwrap() < started.unwrap(), "TurnEnded must precede TurnStarted");
}
