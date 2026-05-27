//! Determinism property tests for `step()`.
//!
//! Contract: same initial `CombatState` + same `DiceRng` seed + same `Action`
//! sequence → identical event stream, identical `post_state_hash`, identical
//! `rng_calls` count.  These tests run each scenario twice and assert
//! bit-identical traces across both runs.
//!
//! Scenarios covered:
//! 1. `det_cast_ap_exhaustion_s6` — Cast drains last AP → S6 auto-EndTurn + AdvanceTurn.
//! 2. `det_dot_tick_during_dead_skip` — EndTurn with DoT on dead-skipped unit.
//! 3. `det_move_with_aoo_reaction` — Move triggers AoO reaction chain (RNG dice roll).
//! 4. `det_phase_transition` — Damage crosses phase threshold → EnterPhase cascade.
//! 5. `det_aoe_multi_target_cast` — AoE fireball hits 3 enemies (per-target ordering).

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::Action,
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, PhaseEntry, StatusBonuses,
        TargetType,
    },
    dice::DiceRng,
    event::Event,
    state::{ActiveStatus, CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    trace::post_state_hash_hex,
    AbilityId, PoolKind, RegenRule, StatusDef, StatusId,
};
use storyforge::game::hex::hex_from_offset;

// ── Minimal ContentView stub ──────────────────────────────────────────────────

struct DeterminismContent {
    abilities: HashMap<AbilityId, AbilityDef>,
    status_defs: HashMap<StatusId, StatusDef>,
}

impl DeterminismContent {
    fn empty() -> Self {
        Self { abilities: HashMap::new(), status_defs: HashMap::new() }
    }

    fn with_ability(mut self, id: &str, def: AbilityDef) -> Self {
        self.abilities.insert(AbilityId::from(id), def);
        self
    }

    fn with_status(mut self, id: StatusId, def: StatusDef) -> Self {
        self.status_defs.insert(id, def);
        self
    }
}

impl ContentView for DeterminismContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.get(id)
    }
    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        self.status_defs.get(id)
    }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
        None
    }
}

// ── Unit builder ─────────────────────────────────────────────────────────────

fn make_unit(id: u64, team: Team, pos_col: i32, pos_row: i32) -> Unit {
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
        auras: vec![],
        enemy_phases: vec![],
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((6, 6)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
    }
}

// ── Trace harness ────────────────────────────────────────────────────────────

/// Per-step trace record collected during a run.
#[derive(Debug, PartialEq)]
struct StepTrace {
    events: Vec<Event>,
    post_hash: String,
    rng_calls: u64,
}

/// Run `actions` through `step()` starting from a fresh clone of `initial_state`,
/// using a fresh `DiceRng` seeded with `seed`.  Returns one `StepTrace` per action.
fn run_once(
    initial_state: &CombatState,
    seed: u64,
    actions: &[Action],
    content: &dyn ContentView,
) -> Vec<StepTrace> {
    let mut state = initial_state.clone();
    let mut rng = DiceRng::with_seed(seed);
    let mut traces = Vec::with_capacity(actions.len());

    for action in actions {
        let (events, ctx) = step(&mut state, action.clone(), &mut rng, content)
            .unwrap_or_else(|e| panic!("step failed: {e:?}"));
        let post_hash = post_state_hash_hex(&state);
        traces.push(StepTrace { events, post_hash, rng_calls: ctx.rng_calls });
    }

    traces
}

/// Assert two trace runs are identical; on mismatch print the first divergent step.
fn assert_traces_identical(a: &[StepTrace], b: &[StepTrace], scenario: &str) {
    assert_eq!(
        a.len(), b.len(),
        "{scenario}: trace length differs ({} vs {})", a.len(), b.len()
    );
    for (idx, (ta, tb)) in a.iter().zip(b.iter()).enumerate() {
        if ta == tb {
            continue;
        }
        // Print structured context for debugging.
        if ta.events != tb.events {
            eprintln!("{scenario} step {idx}: DIVERGED on events");
            eprintln!("  run-A events ({}):", ta.events.len());
            for (i, e) in ta.events.iter().enumerate() {
                eprintln!("    [{i}] {e:?}");
            }
            eprintln!("  run-B events ({}):", tb.events.len());
            for (i, e) in tb.events.iter().enumerate() {
                eprintln!("    [{i}] {e:?}");
            }
        }
        if ta.post_hash != tb.post_hash {
            eprintln!("{scenario} step {idx}: DIVERGED on post_state_hash");
            eprintln!("  run-A: {}", ta.post_hash);
            eprintln!("  run-B: {}", tb.post_hash);
        }
        if ta.rng_calls != tb.rng_calls {
            eprintln!("{scenario} step {idx}: DIVERGED on rng_calls");
            eprintln!("  run-A: {}", ta.rng_calls);
            eprintln!("  run-B: {}", tb.rng_calls);
        }
        panic!("{scenario}: first divergence at step {idx} (see stderr for details)");
    }
}

// ── Scenario helpers ─────────────────────────────────────────────────────────

const SEED: u64 = 42;

// ── Tests ────────────────────────────────────────────────────────────────────

/// Scenario 1: Cast with 1 AP → AP exhausted after cast → S6 auto-EndTurn +
/// AdvanceTurn cascade is emitted deterministically.
#[test]
fn det_cast_ap_exhaustion_s6() {
    let mut actor = make_unit(1, Team::Player, 0, 0);
    // Only 1 AP so the cast exhausts it, triggering S6 auto-end.
    actor.pools[PoolKind::Ap] = Some((1, 1));
    actor.pools[PoolKind::Mp] = Some((0, 6));

    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = CombatState::new(vec![actor, target], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 0);

    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Damage { dice: storyforge::combat_engine::dice::DiceExpr::new(1, 6, 0) }, // scenario 1
        statuses: vec![],
        requires_los: false,
    };
    let content = DeterminismContent::empty().with_ability("strike", ability);

    let actions = vec![Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    }];

    let run_a = run_once(&state, SEED, &actions, &content);
    let run_b = run_once(&state, SEED, &actions, &content);
    assert_traces_identical(&run_a, &run_b, "det_cast_ap_exhaustion_s6");
}

/// Scenario 2: EndTurn while a DoT status is ticking during a dead-unit skip.
/// Queue=[A alive, B dead+DoT-on-C, C alive].  EndTurn(A) advances to B,
/// ticks DoT on C, then skips B.  Exercises DotDamaged + RNG usage.
#[test]
fn det_dot_tick_during_dead_skip() {
    let dot_id = StatusId("poison".to_string());

    let a = make_unit(1, Team::Player, 0, 0);

    let mut b = make_unit(2, Team::Enemy, 1, 0);
    b.hp = 0;  // dead

    let mut c = make_unit(3, Team::Enemy, 2, 0);
    c.statuses.push(ActiveStatus {
        id: dot_id.clone(),
        rounds_remaining: 3,
        dot_per_tick: 4,
        applier: UnitId(2),
    });

    let mut state = CombatState::new(vec![a, b, c], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![UnitId(1), UnitId(2), UnitId(3)], 0);

    let dot_def = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses::default(),
        hp_percent_dot: 0,
    };
    let content = DeterminismContent::empty().with_status(dot_id, dot_def);

    let actions = vec![Action::EndTurn { actor: UnitId(1) }];

    let run_a = run_once(&state, SEED, &actions, &content);
    let run_b = run_once(&state, SEED, &actions, &content);
    assert_traces_identical(&run_a, &run_b, "det_dot_tick_during_dead_skip");
}

/// Scenario 3: Move that disengages from an adjacent enemy → AoO fires → RNG
/// dice roll for attack damage.  Uses real `DiceRng` to exercise non-trivial
/// random branching in a deterministic way.
#[test]
fn det_move_with_aoo_reaction() {
    let mut mover = make_unit(1, Team::Player, 1, 0);
    mover.pools[PoolKind::Mp] = Some((6, 6));

    let mut enemy = make_unit(2, Team::Enemy, 0, 0); // adjacent to (1,0)
    enemy.aoo_dice = Some(storyforge::combat_engine::dice::DiceExpr::new(1, 6, 2));

    let mut state = CombatState::new(vec![mover, enemy], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 0);

    let content = DeterminismContent::empty();

    // Move mover away from the adjacent enemy: (1,0) → (2,0) → (3,0).
    let actions = vec![Action::Move {
        actor: UnitId(1),
        path: vec![hex_from_offset(2, 0), hex_from_offset(3, 0)],
    }];

    let run_a = run_once(&state, SEED, &actions, &content);
    let run_b = run_once(&state, SEED, &actions, &content);
    assert_traces_identical(&run_a, &run_b, "det_move_with_aoo_reaction");
}

/// Scenario 4: Damage crosses enemy phase threshold → `EnterPhase` cascade
/// fires (SetMaxHp + RefreshAggregates + PhaseEntered event).
/// The phase data lives on `Unit.enemy_phases`.
#[test]
fn det_phase_transition() {
    let caster_id = UnitId(1);
    let boss_id = UnitId(2);

    let mut caster = make_unit(1, Team::Player, 0, 0);
    // Give caster 3 AP and mana for the ability
    caster.pools[PoolKind::Ap] = Some((3, 3));
    caster.pools[PoolKind::Mp] = Some((0, 6));

    let mut boss = make_unit(2, Team::Enemy, 1, 0);
    boss.hp = 60;
    boss.max_hp = 100;
    // Phase triggers at 50% HP (hp ≤ 50)
    boss.enemy_phases = vec![PhaseEntry { pct: 50, new_max_hp: 0, heal_to_full: false }];

    let mut state = CombatState::new(vec![caster, boss], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![caster_id, boss_id], 0);

    // Ability: deals 20 raw damage (60 → 40, crosses 50% of 100).
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Damage {
            dice: storyforge::combat_engine::dice::DiceExpr::new(0, 1, 20),
        },
        statuses: vec![],
        requires_los: false,
    };
    let content = DeterminismContent::empty().with_ability("heavy_blow", ability);

    let actions = vec![Action::Cast {
        actor: caster_id,
        ability: AbilityId::from("heavy_blow"),
        target: boss_id,
        target_pos: hex_from_offset(1, 0),
    }];

    let run_a = run_once(&state, SEED, &actions, &content);
    let run_b = run_once(&state, SEED, &actions, &content);
    assert_traces_identical(&run_a, &run_b, "det_phase_transition");
}

/// Scenario 5: Multi-target AoE cast → per-target ordering is stable across
/// both runs.  Three enemies at adjacent positions all take damage.
#[test]
fn det_aoe_multi_target_cast() {
    use storyforge::combat_engine::CasterContext;

    let actor_id = UnitId(1);
    let mut actor = make_unit(1, Team::Player, 0, 0);
    actor.pools[PoolKind::Ap] = Some((2, 2));

    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    let mut ea = make_unit(10, Team::Enemy, 3, 0);
    ea.pos = target_pos;
    let mut eb = make_unit(11, Team::Enemy, 0, 0);
    eb.pos = neighbors[0];
    let mut ec = make_unit(12, Team::Enemy, 0, 0);
    ec.pos = neighbors[1];

    let mut state = CombatState::new(
        vec![actor, ea, eb, ec],
        1, RoundPhase::ActorTurn, SEED,
    );
    state.set_turn_queue(vec![actor_id, UnitId(10), UnitId(11), UnitId(12)], 0);

    // Apply str_mod=2 to actor so caster_context is non-trivial.
    if let Some(u) = state.unit_mut(actor_id) {
        u.caster_context = CasterContext { str_mod: 2, ..Default::default() };
    }

    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 8 },
        target_type: TargetType::Ground,
        aoe: AoEShape::Circle { radius: 1 },
        friendly_fire: false,
        effect: EffectDef::Damage {
            dice: storyforge::combat_engine::dice::DiceExpr::new(1, 6, 0),
        },
        statuses: vec![],
        requires_los: false,
    };
    let content = DeterminismContent::empty().with_ability("fireball", ability);

    let actions = vec![Action::Cast {
        actor: actor_id,
        ability: AbilityId::from("fireball"),
        target: actor_id, // Ground-type: actor is placeholder primary target
        target_pos,
    }];

    let run_a = run_once(&state, SEED, &actions, &content);
    let run_b = run_once(&state, SEED, &actions, &content);
    assert_traces_identical(&run_a, &run_b, "det_aoe_multi_target_cast");
}
