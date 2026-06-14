//! In-process record/replay tests for the engine trace (Phase 5 step 5e).
//!
//! No Bevy, no file I/O — uses `trace::serialize_*` / `parse_*` helpers
//! purely in memory.  Each test follows a fixed pattern:
//!
//! 1. **Record phase:** build state + content, drive `step()`, construct
//!    `InitLine` + `StepLine`s via `serialize_*`.  Stored as `Vec<String>`.
//! 2. **Replay phase:** parse each line, reconstruct `CombatState` + `DiceRng`
//!    from the `InitLine`, replay each `step()`.  Assert per-step:
//!    - `events` byte-equal,
//!    - `rng_calls` match,
//!    - `post_state_hash` matches.
//!
//! Gate criteria: #4 (canonical scenarios), #9 (size benchmark).

#![allow(clippy::field_reassign_with_default)]

use hexx::Hex;

use storyforge::combat_engine::{
    action::Action,
    content::ContentView, // used by record_then_replay signature
    event::Event,
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    trace::{
        parse_init, parse_step, post_state_hash_hex, serialize_init, serialize_step, InitLine,
        StepLine, SCHEMA_VERSION,
    },
    AbilityDef,
    AbilityId,
    AbilityRange,
    AoEShape,
    DiceExpr,
    DiceRng,
    EffectDef,
    PhaseEntry,
    StatusId,
    TargetType,
};

#[allow(dead_code)]
fn sid(s: &str) -> StatusId {
    StatusId(s.to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

const SEED: u64 = 0xDEAD_BEEF_1234_5678;

fn uid(n: u64) -> UnitId {
    UnitId(n)
}
fn abid(s: &str) -> AbilityId {
    AbilityId(s.to_string())
}

use crate::common::engine_unit::{template, EngineUnitBuilder, StubContent};

/// speed=4, Ap=3, Mp=6 — replay defaults.
fn make_unit(id: u64, team: Team, hp: i32, max_hp: i32, pos: Hex) -> Unit {
    EngineUnitBuilder::new(id)
        .team(team)
        .pos_hex(pos)
        .hp(hp, max_hp)
        .speed(4)
        .ap(3, 3)
        .build() // Mp default is (6,6) — matches
}

/// Build `InitLine` from a `CombatState` and a seed.
fn init_line_for(state: &CombatState, seed: u64) -> InitLine {
    InitLine {
        schema: SCHEMA_VERSION,
        session_id: "replay_test".to_owned(),
        rng_seed: seed,
        units: state.units().to_vec(),
        next_synthetic_uid: state.next_synthetic_uid(),
        round: state.round,
        phase: state.phase,
        turn_queue: state.turn_queue.clone(),
        content_hash: "blake3:test".to_owned(),
    }
}

/// Build `CombatState` from an `InitLine`.
fn state_from_init(init: &InitLine) -> CombatState {
    let mut state = CombatState::new(init.units.clone(), init.round, init.phase, init.rng_seed);
    state.set_turn_queue(init.turn_queue.order.clone(), init.turn_queue.index);
    state.set_next_synthetic_uid(init.next_synthetic_uid);
    state
}

/// Record then replay a sequence of actions. Returns `Ok(Vec<StepLine>)` with
/// the recorded lines (for inspection), or panics on any assertion failure.
fn record_then_replay(
    mut state: CombatState,
    seed: u64,
    content: &dyn ContentView,
    actions: Vec<Action>,
) -> Vec<StepLine> {
    // ── Record ──────────────────────────────────────────────────────────────
    let init = init_line_for(&state, seed);
    let mut rng = DiceRng::with_seed(seed);

    let mut recorded_jsonl: Vec<String> = vec![serialize_init(&init).expect("serialize init")];
    let mut step_lines: Vec<StepLine> = vec![];

    for (idx, action) in actions.into_iter().enumerate() {
        let (events, ctx) = step(&mut state, action.clone(), &mut rng, content)
            .unwrap_or_else(|e| panic!("step {idx} failed: {e:?}"));
        let hash = post_state_hash_hex(&state);
        let line = StepLine {
            schema: SCHEMA_VERSION,
            step: idx as u64,
            action,
            events,
            rng_calls: ctx.rng_calls,
            post_state_hash: hash,
        };
        recorded_jsonl.push(serialize_step(&line).expect("serialize step"));
        step_lines.push(line);
    }

    // ── Replay ──────────────────────────────────────────────────────────────
    let mut lines_iter = recorded_jsonl.iter();

    let init_json = lines_iter.next().expect("init line");
    let parsed_init = parse_init(init_json).expect("parse init");

    let mut replay_state = state_from_init(&parsed_init);
    let mut replay_rng = DiceRng::with_seed(parsed_init.rng_seed);

    for (idx, step_json) in lines_iter.enumerate() {
        let recorded = parse_step(step_json).expect("parse step");

        let (live_events, live_ctx) = step(
            &mut replay_state,
            recorded.action.clone(),
            &mut replay_rng,
            content,
        )
        .unwrap_or_else(|e| panic!("replay step {idx} failed: {e:?}"));

        assert_eq!(
            live_events, recorded.events,
            "step {idx}: events diverged\nrecorded: {:?}\nlive: {:?}",
            recorded.events, live_events
        );
        assert_eq!(
            live_ctx.rng_calls, recorded.rng_calls,
            "step {idx}: rng_calls diverged (recorded={} live={})",
            recorded.rng_calls, live_ctx.rng_calls
        );
        let live_hash = post_state_hash_hex(&replay_state);
        assert_eq!(
            live_hash, recorded.post_state_hash,
            "step {idx}: post_state_hash diverged\nrecorded: {}\nlive: {}",
            recorded.post_state_hash, live_hash
        );
    }

    step_lines
}

// ── Scenario 1: pure move, no enemies ─────────────────────────────────────────

/// A single player unit moves along a 3-hex path. No AoO possible.
#[test]
fn replay_pure_move_no_enemies() {
    let unit = make_unit(1, Team::Player, 20, 20, Hex::new(0, 0));
    let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![uid(1)], 0);

    let path = vec![Hex::new(0, 0), Hex::new(1, 0), Hex::new(2, 0)];
    let actions = vec![Action::Move {
        actor: uid(1),
        path,
    }];

    let lines = record_then_replay(state, SEED, &StubContent::new(), actions);
    assert_eq!(lines.len(), 1);
    // A move of length 2 uses 0 RNG calls.
    assert_eq!(lines[0].rng_calls, 0);
}

// ── Scenario 2: move with AoO chain ───────────────────────────────────────────

/// Actor moves away from an adjacent armed enemy, triggering an AoO.
/// The enemy has `aoo_dice` set so the AoO fires and consumes RNG calls.
///
/// Setup mirrors `tests/combat_engine/reaction.rs::aoo_triggers_on_disengage`:
/// mover at (0,0), enemy at (1,0) — hex distance 1 (adjacent).
/// Destination (-1,0) — distance 2 from enemy, so AoO fires on first step.
#[test]
fn replay_move_with_aoo_chain() {
    // Hex::new uses axial (q,r) coords. Distance((0,0),(1,0)) = 1 → adjacent.
    let mut mover = make_unit(1, Team::Player, 20, 20, Hex::new(0, 0));
    mover.reactions_left = 1;

    let mut enemy = make_unit(2, Team::Enemy, 20, 20, Hex::new(1, 0));
    enemy.aoo_dice = Some(DiceExpr::new(1, 4, 0));
    enemy.reactions_left = 1;

    let mut state = CombatState::new(vec![mover, enemy], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![uid(1), uid(2)], 0);

    // Stepping from (0,0) → (-1,0): distance from (-1,0) to enemy (1,0) = 2
    // → no longer adjacent → AoO fires. Single-step path to keep it simple.
    let path = vec![Hex::new(0, 0), Hex::new(-1, 0)];
    let actions = vec![Action::Move {
        actor: uid(1),
        path,
    }];

    let lines = record_then_replay(state, SEED, &StubContent::new(), actions);
    assert_eq!(lines.len(), 1);
    // AoO fires dice → rng_calls > 0
    assert!(
        lines[0].rng_calls > 0,
        "AoO should consume RNG calls; got {}",
        lines[0].rng_calls
    );
}

// ── Scenario 3: cast damage (basic) ───────────────────────────────────────────

/// Actor casts a damage ability on an enemy. RNG dice roll consumed.
#[test]
fn replay_cast_damage_basic() {
    let actor_id = uid(1);
    let target_id = uid(2);

    let mut actor = make_unit(1, Team::Player, 20, 20, Hex::new(0, 0));
    // weapon dice for crit-fail d20 roll
    actor.caster_context.weapon_dice = Some(DiceExpr::new(1, 6, 0));

    let target = make_unit(2, Team::Enemy, 20, 20, Hex::new(1, 0));

    let mut state = CombatState::new(vec![actor, target], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![actor_id, target_id], 0);

    let ability_id = abid("strike");
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 3 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        effect: EffectDef::Damage {
            dice: DiceExpr::new(1, 6, 2),
        },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    };

    let content = StubContent::new().with_ability(ability_id.0.clone(), ability.clone());

    let actions = vec![Action::Cast {
        actor: actor_id,
        ability: ability_id,
        target: target_id,
        target_pos: Hex::new(1, 0),
    }];

    let lines = record_then_replay(state, SEED, &content, actions);
    assert_eq!(lines.len(), 1);
    // Cast uses at least 1 RNG call (d20 crit-fail check)
    assert!(
        lines[0].rng_calls >= 1,
        "cast must use RNG; got {}",
        lines[0].rng_calls
    );
    // Must have a UnitDamaged event
    assert!(
        lines[0]
            .events
            .iter()
            .any(|e| matches!(e, Event::UnitDamaged { .. })),
        "expected UnitDamaged in {:?}",
        lines[0].events
    );
}

// ── Scenario 4: phase trigger ──────────────────────────────────────────────────

/// A boss with a 50% phase threshold. Damage cast crosses the threshold;
/// trace must record `Event::PhaseEntered` deterministically.
#[test]
fn replay_phase_trigger() {
    let attacker_id = uid(1);
    let boss_id = uid(2);

    let mut attacker = make_unit(1, Team::Player, 30, 30, Hex::new(0, 0));
    attacker.caster_context.weapon_dice = Some(DiceExpr::new(1, 6, 0));

    // Boss: 100 hp, 50% threshold at 50 hp. Fixed damage of 60 will cross it.
    let mut boss = make_unit(2, Team::Enemy, 100, 100, Hex::new(1, 0));
    boss.enemy_phases = vec![PhaseEntry {
        pct: 50,
        new_max_hp: 150,
        heal_to_full: false,
        tags: None,
        runtime: None,
    }];

    let mut state = CombatState::new(vec![attacker, boss], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![attacker_id, boss_id], 0);

    // Use a high fixed-bonus ability so damage definitely crosses 50% threshold.
    let ability_id = abid("heavy_blow");
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 3 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        // 1d6 + 60 will always exceed 50 hp out of 100
        effect: EffectDef::Damage {
            dice: DiceExpr::new(1, 6, 60),
        },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    };

    let content = StubContent::new().with_ability(ability_id.0.clone(), ability);

    let actions = vec![Action::Cast {
        actor: attacker_id,
        ability: ability_id,
        target: boss_id,
        target_pos: Hex::new(1, 0),
    }];

    let lines = record_then_replay(state, SEED, &content, actions);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0]
            .events
            .iter()
            .any(|e| matches!(e, Event::PhaseEntered { .. })),
        "expected PhaseEntered in {:?}",
        lines[0].events
    );
}

// ── Scenario 5: EndTurn advances queue ────────────────────────────────────────

/// `Action::EndTurn` moves turn_queue.index; the recorded `turn_queue.index`
/// in `post_state_hash` advances; replay matches.
#[test]
fn replay_endturn_advances_queue() {
    let mut state = CombatState::new(
        vec![
            make_unit(1, Team::Player, 20, 20, Hex::ZERO),
            make_unit(2, Team::Enemy, 20, 20, Hex::new(3, 0)),
        ],
        1,
        RoundPhase::ActorTurn,
        SEED,
    );
    state.set_turn_queue(vec![uid(1), uid(2)], 0);

    let actions = vec![Action::EndTurn { actor: uid(1) }];
    let lines = record_then_replay(state, SEED, &StubContent::new(), actions);
    assert_eq!(lines.len(), 1);
    // EndTurn uses no RNG
    assert_eq!(lines[0].rng_calls, 0);
    // Must emit TurnEnded + TurnStarted
    assert!(lines[0]
        .events
        .iter()
        .any(|e| matches!(e, Event::TurnEnded { .. })));
    assert!(lines[0]
        .events
        .iter()
        .any(|e| matches!(e, Event::TurnStarted { .. })));
}

// ── Divergence sentinels (gate honesty) ──────────────────────────────────────

/// Tampering with a recorded `events[]` entry is detected by replay.
#[test]
#[should_panic(expected = "events diverged")]
fn replay_event_divergence_detected() {
    let unit = make_unit(1, Team::Player, 20, 20, Hex::new(0, 0));
    let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![uid(1)], 0);

    let content = StubContent::new();
    let path = vec![Hex::new(0, 0), Hex::new(1, 0)];
    let init = init_line_for(&state, SEED);
    let mut rng = DiceRng::with_seed(SEED);

    let (events, ctx) = step(
        &mut state,
        Action::Move {
            actor: uid(1),
            path: path.clone(),
        },
        &mut rng,
        &content,
    )
    .unwrap();
    let hash = post_state_hash_hex(&state);

    // Record with tampered events (append a spurious extra event).
    let mut tampered_events = events.clone();
    tampered_events.push(Event::UnitDied { unit: uid(99) });

    let step_line = StepLine {
        schema: SCHEMA_VERSION,
        step: 0,
        action: Action::Move {
            actor: uid(1),
            path,
        },
        events: tampered_events,
        rng_calls: ctx.rng_calls,
        post_state_hash: hash,
    };
    let init_json = serialize_init(&init).unwrap();
    let step_json = serialize_step(&step_line).unwrap();

    // Replay: must detect divergence.
    let parsed_init = parse_init(&init_json).unwrap();
    let mut replay_state = state_from_init(&parsed_init);
    let mut replay_rng = DiceRng::with_seed(parsed_init.rng_seed);

    let recorded = parse_step(&step_json).unwrap();
    let (live_events, _live_ctx) = step(
        &mut replay_state,
        recorded.action.clone(),
        &mut replay_rng,
        &content,
    )
    .unwrap();

    assert_eq!(live_events, recorded.events, "events diverged");
}

/// Tampering with a recorded `rng_calls` value is detected by replay.
#[test]
#[should_panic(expected = "rng_calls diverged")]
fn replay_rng_count_divergence_detected() {
    let mut attacker = make_unit(1, Team::Player, 20, 20, Hex::new(0, 0));
    attacker.caster_context.weapon_dice = Some(DiceExpr::new(1, 6, 0));
    let target = make_unit(2, Team::Enemy, 20, 20, Hex::new(1, 0));

    let mut state = CombatState::new(vec![attacker, target], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![uid(1), uid(2)], 0);

    let ability_id = abid("strike");
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 3 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        effect: EffectDef::Damage {
            dice: DiceExpr::new(1, 6, 0),
        },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    };

    let content = StubContent::new().with_ability(ability_id.0.clone(), ability);
    let init = init_line_for(&state, SEED);
    let mut rng = DiceRng::with_seed(SEED);

    let action = Action::Cast {
        actor: uid(1),
        ability: ability_id,
        target: uid(2),
        target_pos: Hex::new(1, 0),
    };
    let (events, ctx) = step(&mut state, action.clone(), &mut rng, &content).unwrap();
    let hash = post_state_hash_hex(&state);

    // Tamper: add 99 to rng_calls.
    let step_line = StepLine {
        schema: SCHEMA_VERSION,
        step: 0,
        action: action.clone(),
        events,
        rng_calls: ctx.rng_calls + 99, // <-- tampered
        post_state_hash: hash,
    };
    let init_json = serialize_init(&init).unwrap();
    let step_json = serialize_step(&step_line).unwrap();

    // Replay.
    let parsed_init = parse_init(&init_json).unwrap();
    let mut replay_state = state_from_init(&parsed_init);
    let mut replay_rng = DiceRng::with_seed(parsed_init.rng_seed);

    let recorded = parse_step(&step_json).unwrap();
    let (_live_events, live_ctx) = step(
        &mut replay_state,
        recorded.action.clone(),
        &mut replay_rng,
        &content,
    )
    .unwrap();

    assert_eq!(
        live_ctx.rng_calls, recorded.rng_calls,
        "rng_calls diverged (recorded={} live={})",
        recorded.rng_calls, live_ctx.rng_calls
    );
}

// ── replay_summon_initiative_hash_stable ──────────────────────────────────────

/// Replay-determinism for a summon step: the in-step d20 initiative roll is
/// captured in `rng_calls` and the `post_state_hash` is identical on replay.
#[test]
fn replay_summon_initiative_hash_stable() {
    // high dex so total is distinct from the raw roll
    let mut minion_tpl = template();
    minion_tpl.max_hp = 6;
    minion_tpl.caster_context.dex_mod = 4;
    let summon_ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 0 },
        target_type: TargetType::Myself,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Summon {
            template_id: "minion".into(),
            max_active: None,
        },
        statuses: vec![],
        requires_los: false,
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    };
    let content = StubContent::new()
        .with_ability("summon", summon_ability)
        .with_template("minion", minion_tpl);

    let summoner = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos_hex(Hex::new(0, 0))
        .initiative(10)
        .ap(2, 2)
        .build();
    let enemy = EngineUnitBuilder::new(2)
        .team(Team::Enemy)
        .pos_hex(Hex::new(5, 0))
        .initiative(5)
        .build();

    let mut state = CombatState::new(vec![summoner, enemy], 1, RoundPhase::ActorTurn, SEED);
    state.set_turn_queue(vec![uid(1), uid(2)], 0);

    let action = Action::Cast {
        actor: uid(1),
        ability: abid("summon"),
        target: uid(1),
        target_pos: Hex::new(0, 0),
    };

    record_then_replay(state, SEED, &content, vec![action]);
}
