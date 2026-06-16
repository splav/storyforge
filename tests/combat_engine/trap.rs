//! Commit B: environmental-trap trigger in the `step` pump loop.
//!
//! A trap is an `EnvObject { kind: Hazard }` in `CombatState.environment`.
//! Entering its hex during a `Move` fires the trap's `AbilityId` through the
//! Cast effect fanout, sourced by `EffectSource::Env`. Tests drive the public
//! `step(Action::Move)` (the trigger lives in `step_inner`).

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::Action,
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, PassiveTrigger,
        StatusApplication, StatusOn, TargetType,
    },
    dice::ExpectedValue,
    event::Event,
    state::{
        CombatState, EffectSource, EnvId, EnvKind, EnvObject, RoundPhase, Team, TeamSet, Unit,
        UnitId,
    },
    step::step,
    AbilityId, DiceExpr, PoolKind, StatusBonuses, StatusDef, StatusId,
};
use storyforge::game::hex::hex_from_offset;

// ── Harness ─────────────────────────────────────────────────────────────────────

struct Stub(HashMap<AbilityId, AbilityDef>, StatusDef);
impl Stub {
    fn new(id: &str, def: AbilityDef) -> Self {
        Self(
            HashMap::from([(AbilityId::from(id), def)]),
            StatusDef {
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                forces_targeting: false,
                skips_turn: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                bonuses: StatusBonuses {
                    runtime: storyforge::combat_engine::RuntimeStatsDelta(Default::default()),
                },
                ..Default::default()
            },
        )
    }
}
impl ContentView for Stub {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.0.get(id)
    }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
        Some(&self.1)
    }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
        None
    }
}

use crate::common::engine_unit::EngineUnitBuilder;

fn unit(id: u64, team: Team, col: i32, hp: i32, rage: Option<(i32, i32)>) -> Unit {
    use storyforge::combat_engine::RegenRule;
    let mut b = EngineUnitBuilder::new(id)
        .team(team)
        .pos(col, 0)
        .hp(hp, 20)
        // Mana regen is None in trap tests (not the canonical Increment(1))
        .regen(PoolKind::Mana, RegenRule::None)
        .regen(PoolKind::Energy, RegenRule::None);
    if let Some((cur, max)) = rage {
        b = b.rage(cur, max);
    }
    b.build()
}

/// `n` damage (n×d1 = deterministic n), plus optional status on the victim.
fn trap_ability(n: u32, status: Option<&str>) -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 0,
        costs: vec![],
        range: AbilityRange { min: 0, max: 0 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Damage {
            dice: DiceExpr::new(n, 1, 0),
        },
        statuses: status
            .map(|s| {
                vec![StatusApplication {
                    status: StatusId::from(s),
                    on: StatusOn::Target,
                    duration_rounds: 2,
                }]
            })
            .unwrap_or_default(),
        requires_los: false,
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
        power: None,
    }
}

fn hazard(id: u32, col: i32) -> EnvObject {
    EnvObject {
        id: EnvId(id),
        hex: hex_from_offset(col, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: None,
        revealed_to: TeamSet::EMPTY,
    }
}

/// Run a single-actor move over `cols` and return (state, events).
/// Returns `(state, events, interrupted)` — `interrupted` mirrors `ctx.interrupted`
/// from the engine step.
fn run(
    mover: Unit,
    env: Vec<EnvObject>,
    cols: &[i32],
    content: &Stub,
) -> (CombatState, Vec<Event>, bool) {
    let mut state = CombatState::new(vec![mover], 1, RoundPhase::ActorTurn, 0);
    state.environment = env;
    let path = cols.iter().map(|&c| hex_from_offset(c, 0)).collect();
    let (events, ctx) = step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path,
        },
        &mut ExpectedValue,
        content,
    )
    .expect("move should succeed");
    (state, events, ctx.interrupted)
}

fn hp(s: &CombatState, id: u64) -> i32 {
    s.unit(UnitId(id)).map(|u| u.hp()).unwrap_or(-1)
}

// ── Tests ───────────────────────────────────────────────────────────────────────

/// Entering a hazard deals its damage, removes the trap (one-shot), and emits
/// HazardTriggered — for any team (symmetry). Firing is NOT a reveal.
#[test]
fn trap_triggers_damages_and_disappears() {
    for team in [Team::Player, Team::Enemy] {
        let (s, ev, _) = run(
            unit(1, team, 0, 10, None),
            vec![hazard(7, 1)],
            &[1],
            &Stub::new("trap", trap_ability(2, None)),
        );
        assert_eq!(hp(&s, 1), 8, "trap (2 dmg) for {team:?}");
        assert!(s.environment.is_empty(), "trap disappears after firing");
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::HazardTriggered { victim, .. } if *victim == UnitId(1))));
        assert!(
            !ev.iter().any(|e| matches!(e, Event::EnvRevealed { .. })),
            "firing must not emit EnvRevealed"
        );
    }
}

/// A fired trap is removed; re-entering its hex does nothing.
#[test]
fn trap_gone_after_firing() {
    let content = Stub::new("trap", trap_ability(2, None));
    let mut state = CombatState::new(
        vec![unit(1, Team::Player, 0, 10, None)],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.environment = vec![hazard(1, 1)];
    // Pass through (1,0)->(2,0): fires + removes the trap.
    step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)],
        },
        &mut ExpectedValue,
        &content,
    )
    .expect("first move");
    assert_eq!(hp(&state, 1), 8);
    assert!(state.environment.is_empty(), "trap removed after firing");
    // Step back onto the now-clear hex: no damage.
    step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path: vec![hex_from_offset(1, 0)],
        },
        &mut ExpectedValue,
        &content,
    )
    .expect("second move");
    assert_eq!(hp(&state, 1), 8, "no trap remains to fire");
}

/// Env-sourced damage grants no rage to the source (no phantom unit/panic);
/// the victim still gains rage from being hurt.
#[test]
fn trap_no_rage_to_env_but_victim_gains() {
    let (s, _, _) = run(
        unit(1, Team::Player, 0, 10, Some((0, 10))),
        vec![hazard(1, 1)],
        &[1],
        &Stub::new("trap", trap_ability(2, None)),
    );
    let rage = s.unit(UnitId(1)).unwrap().pools[PoolKind::Rage]
        .map(|(c, _)| c)
        .unwrap();
    assert_eq!(rage, 1, "victim gains rage; env source grants none");
}

/// A lethal trap kills the mover on the trap hex and truncates the rest of the
/// path (existing dead-actor guard).
#[test]
fn trap_lethal_truncates_path() {
    let (s, ev, _) = run(
        unit(1, Team::Player, 0, 2, None),
        vec![hazard(1, 1)],
        &[1, 2],
        &Stub::new("trap", trap_ability(2, None)),
    );
    assert_eq!(hp(&s, 1), 0);
    assert_eq!(
        s.unit(UnitId(1)).unwrap().pos,
        hex_from_offset(1, 0),
        "dead mover doesn't advance past the trap"
    );
    assert!(ev
        .iter()
        .any(|e| matches!(e, Event::UnitDied { unit } if *unit == UnitId(1))));
}

/// A trap that applies a status uses an Env applier, not a phantom unit.
#[test]
fn trap_status_uses_env_applier() {
    let (s, _, _) = run(
        unit(1, Team::Player, 0, 10, None),
        vec![hazard(5, 1)],
        &[1],
        &Stub::new("trap", trap_ability(1, Some("disoriented"))),
    );
    let st = s
        .unit(UnitId(1))
        .unwrap()
        .statuses
        .iter()
        .find(|s| s.id == StatusId::from("disoriented"))
        .expect("status applied");
    assert_eq!(st.applier, EffectSource::Env(EnvId(5)));
}

/// `CombatState.environment` round-trips through serde (crate-level roundtrip
/// tests are dead, so cover the new field here).
#[test]
fn env_round_trips_through_serde() {
    let mut state = CombatState::new(
        vec![unit(1, Team::Player, 0, 10, None)],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.environment = vec![hazard(2, 1), hazard(1, 3)];
    let decoded: CombatState =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    // Serialized sorted by id; both objects survive the round-trip.
    assert_eq!(decoded.environment.len(), 2);
    assert!(decoded
        .environment
        .iter()
        .any(|e| e.id == EnvId(1) && e.hex == hex_from_offset(3, 0)));
    assert!(decoded.environment.iter().any(|e| e.id == EnvId(2)));
}

// ── Passive reveal (Kael "scout_traps") ─────────────────────────────────────────

/// A `RevealEnvInRange` passive with `TurnStart` reveals hidden hazards within
/// range only, and is idempotent (a second resolve emits no new EnvRevealed).
#[test]
fn passive_reveal_in_range_only_and_idempotent() {
    let scout = AbilityDef {
        passive: vec![PassiveTrigger::TurnStart],
        effect: EffectDef::RevealEnvInRange { range: 2 },
        ..AbilityDef::default()
    };
    let content = Stub::new("scout", scout);

    let mut u = unit(1, Team::Player, 0, 10, None);
    u.passives = vec![AbilityId::from("scout")];
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    // caster at (0,0): hazard 10 at col 1 (in range), hazard 20 at col 5 (out).
    state.environment = vec![hazard(10, 1), hazard(20, 5)];

    let ev = state.resolve_turn_start_passives(UnitId(1), &content);

    let visible = |id: u32| {
        state
            .environment
            .iter()
            .find(|e| e.id == EnvId(id))
            .unwrap()
            .visible_to(Team::Player)
    };
    assert!(visible(10), "in-range hazard must be revealed to player");
    assert!(
        !visible(20),
        "out-of-range hazard must stay hidden from player"
    );
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(10)))
            .count(),
        1,
        "exactly one EnvRevealed for the in-range hazard",
    );
    assert!(!ev
        .iter()
        .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(20))));

    // Idempotent: re-firing reveals nothing new.
    let ev2 = state.resolve_turn_start_passives(UnitId(1), &content);
    assert!(
        ev2.is_empty(),
        "already-revealed hazards emit no new EnvRevealed"
    );
}

/// A unit without the passive ability reveals nothing.
#[test]
fn no_passive_no_reveal() {
    let content = Stub::new(
        "scout",
        AbilityDef {
            passive: vec![PassiveTrigger::TurnStart],
            effect: EffectDef::RevealEnvInRange { range: 5 },
            ..AbilityDef::default()
        },
    );
    let u = unit(1, Team::Player, 0, 10, None); // passives empty by default
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(10, 1)];

    let ev = state.resolve_turn_start_passives(UnitId(1), &content);
    assert!(ev.is_empty(), "no passive → no events");
    assert!(
        !state.environment[0].visible_to(Team::Player),
        "no passive → hazard stays hidden from player"
    );
}

/// The passive fires through the real turn-start entry (`start_actor_turn`) and
/// consumes zero RNG (deterministic hex-distance scan).
#[test]
fn passive_reveal_via_start_actor_turn_zero_rng() {
    let content = Stub::new(
        "scout",
        AbilityDef {
            passive: vec![PassiveTrigger::TurnStart],
            effect: EffectDef::RevealEnvInRange { range: 2 },
            ..AbilityDef::default()
        },
    );
    let mut u = unit(1, Team::Player, 0, 10, None);
    u.passives = vec![AbilityId::from("scout")];
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(10, 1)];

    let events = state.start_actor_turn(UnitId(1), &content);
    assert!(
        state.environment[0].visible_to(Team::Player),
        "start_actor_turn must fire the reveal passive"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(10))));
}

/// Regression: Vec-form passive + aoe-sourced radius: ability with
/// `passive = vec![TurnStart]` and `aoe = Circle{radius:2}` correctly reveals
/// a hidden hazard within 2 hexes and nothing beyond (aoe_radius is the sole
/// source of the reveal range — no separate reveal_range field).
#[test]
fn turn_start_reveal_still_fires_aoe_radius_source() {
    use storyforge::combat_engine::content::aoe_radius;
    let def = AbilityDef {
        passive: vec![PassiveTrigger::TurnStart],
        aoe: AoEShape::Circle { radius: 2 },
        // range is populated from aoe_radius — mirrors what the TOML parsers do.
        effect: EffectDef::RevealEnvInRange {
            range: aoe_radius(&AbilityDef {
                aoe: AoEShape::Circle { radius: 2 },
                ..AbilityDef::default()
            }),
        },
        target_type: TargetType::Environment,
        ..AbilityDef::default()
    };
    assert_eq!(
        aoe_radius(&def),
        2,
        "aoe_radius must extract 2 from Circle{{radius:2}}"
    );

    let content = Stub::new("scout_env", def);
    let mut u = unit(1, Team::Player, 0, 10, None);
    u.passives = vec![AbilityId::from("scout_env")];
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    // Hazard within range (distance 1) — must be revealed.
    state.environment = vec![hazard(10, 1), hazard(11, 3)];

    let events = state.start_actor_turn(UnitId(1), &content);
    assert!(
        state.environment[0].visible_to(Team::Player),
        "hazard within range-2 must be revealed to player"
    );
    assert!(
        !state.environment[1].visible_to(Team::Player),
        "hazard at distance-3 must NOT be revealed to player"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(10))));
    assert!(!events
        .iter()
        .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(11))));
}

// ── Wave-1: trap halts movement ──────────────────────────────────────────────

/// A trap on an intermediate hex: fires on arrival even when non-lethal, halts
/// the mover at the trap hex (not the requested dest), and sets `ctx.interrupted`.
#[test]
fn trap_on_arrival_triggers_halts_and_sets_interrupted() {
    // mover at col=0, trap at col=1, dest at col=2. Non-lethal (2 dmg, 10 hp).
    let (s, ev, interrupted) = run(
        unit(1, Team::Player, 0, 10, None),
        vec![hazard(1, 1)],
        &[1, 2],
        &Stub::new("trap", trap_ability(2, None)),
    );

    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::HazardTriggered { victim, .. } if *victim == UnitId(1))),
        "HazardTriggered must fire"
    );
    assert_eq!(
        s.unit(UnitId(1)).unwrap().pos,
        hex_from_offset(1, 0),
        "mover must stop AT the trap hex, not continue past it"
    );
    assert_eq!(hp(&s, 1), 8, "trap dealt 2 damage");
    assert!(interrupted, "trap trigger must set ctx.interrupted = true");
}

// ── Wave-3: on-move reveal ───────────────────────────────────────────────────

/// Helper: build an ability with an OnMove reveal passive and Circle radius.
fn on_move_reveal_ability(radius: u32) -> AbilityDef {
    use storyforge::combat_engine::content::aoe_radius;
    AbilityDef {
        passive: vec![PassiveTrigger::OnMove],
        aoe: AoEShape::Circle { radius },
        effect: EffectDef::RevealEnvInRange {
            range: aoe_radius(&AbilityDef {
                aoe: AoEShape::Circle { radius },
                ..AbilityDef::default()
            }),
        },
        target_type: TargetType::Environment,
        ..AbilityDef::default()
    }
}

/// A unit with an OnMove reveal passive moves a multi-hex path. On the step
/// that first brings a hidden hazard within radius 2, the hazard is revealed
/// and the move halts (EnvRevealed is non-benign). The unit stops at that
/// intermediate hex. HazardTriggered is NOT emitted (reveal ≠ fire).
/// ctx.interrupted == true.
#[test]
fn reveal_on_move_halts_and_truncates() {
    let reveal_def = on_move_reveal_ability(2u32);

    // Stub content: one ability for the reveal passive; trap ability reuses
    // a separate "trap" entry but no trap fires in this test (revealed ≠ stepped on).
    let mut abilities = HashMap::new();
    abilities.insert(AbilityId::from("scout"), reveal_def);
    // Minimal trap ability (spike, 0 dmg — should never fire in this test).
    abilities.insert(
        AbilityId::from("trap"),
        AbilityDef {
            effect: EffectDef::Damage {
                dice: DiceExpr::new(0, 1, 0),
            },
            ..AbilityDef::default()
        },
    );

    struct Multi(
        HashMap<AbilityId, AbilityDef>,
        storyforge::combat_engine::StatusDef,
    );
    impl ContentView for Multi {
        fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
            self.0.get(id)
        }
        fn status_def(&self, _: &StatusId) -> Option<&storyforge::combat_engine::StatusDef> {
            Some(&self.1)
        }
        fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
            None
        }
    }
    let content = Multi(
        abilities,
        storyforge::combat_engine::StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: false,
            hp_percent_dot: 0,
            heal_per_tick: 0,
            bonuses: storyforge::combat_engine::StatusBonuses {
                runtime: storyforge::combat_engine::RuntimeStatsDelta(Default::default()),
            },
            ..Default::default()
        },
    );

    // Scout at col=0 with 6 MP; hazard at col=3 (distance 2 from col=1, radius=2).
    // Path: 0→1→2→3→4→5. On step to col=1, hazard at col=3 enters radius 2 → reveal + halt.
    use storyforge::combat_engine::RegenRule;
    let mut scout = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos(0, 0)
        .hp(10, 10)
        .mp(6, 6)
        .regen(PoolKind::Mana, RegenRule::None)
        .regen(PoolKind::Energy, RegenRule::None)
        .build();
    scout.passives = vec![AbilityId::from("scout")];

    let trap_env = EnvObject {
        id: EnvId(42),
        hex: hex_from_offset(3, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: None,
        revealed_to: TeamSet::EMPTY,
    };

    let mut state = CombatState::new(vec![scout], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![trap_env];

    let path: Vec<_> = (1..=5).map(|c| hex_from_offset(c, 0)).collect();
    let (events, ctx) = step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path,
        },
        &mut storyforge::combat_engine::dice::ExpectedValue,
        &content,
    )
    .expect("move must succeed");

    // Halted at col=1 (first hex where hazard at col=3 is within radius 2 — distance exactly 2).
    let final_pos = state.unit(UnitId(1)).unwrap().pos;
    assert_eq!(
        final_pos,
        hex_from_offset(1, 0),
        "scout halts at col=1 when hazard at col=3 enters reveal range (distance=2)"
    );
    assert_ne!(
        final_pos,
        hex_from_offset(5, 0),
        "did not reach original dest"
    );

    // EnvRevealed emitted exactly once.
    let revealed_count = events
        .iter()
        .filter(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(42)))
        .count();
    assert_eq!(revealed_count, 1, "exactly one EnvRevealed for the hazard");

    // Hazard is now revealed in state.
    assert!(state
        .environment
        .iter()
        .find(|e| e.id == EnvId(42))
        .unwrap()
        .visible_to(Team::Player));

    // Hazard still present on the board (not triggered / removed).
    assert!(
        !state.environment.is_empty(),
        "hazard still exists (not triggered)"
    );

    // HazardTriggered NOT emitted (revealed, didn't step on it).
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::HazardTriggered { .. })),
        "reveal must not emit HazardTriggered"
    );

    // MP: consumed for 1 completed step (col 0→1), 5 MP remain.
    let mp = state.unit(UnitId(1)).unwrap().pools[PoolKind::Mp]
        .map(|(c, _)| c)
        .unwrap();
    assert_eq!(mp, 5, "only 1 MP consumed for the 1 completed step");

    // ctx.interrupted == true.
    assert!(
        ctx.interrupted,
        "EnvRevealed must set ctx.interrupted = true"
    );
}

/// Pins the reveal distance metric: a hidden hazard at even-r distance exactly
/// 2 from the step's arrival hex IS revealed; at distance 3 it is NOT.
#[test]
fn reveal_distance_metric() {
    use storyforge::combat_engine::RegenRule;
    let reveal_def = on_move_reveal_ability(2u32);

    let mut abilities = HashMap::new();
    abilities.insert(AbilityId::from("scout"), reveal_def);
    // Distance-2 hazard at col=3 (arrival=col=1, dist=2); distance-3 at col=4.
    abilities.insert(
        AbilityId::from("trap"),
        AbilityDef {
            effect: EffectDef::Damage {
                dice: DiceExpr::new(0, 1, 0),
            },
            ..AbilityDef::default()
        },
    );

    struct Multi(HashMap<AbilityId, AbilityDef>);
    impl ContentView for Multi {
        fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
            self.0.get(id)
        }
        fn status_def(&self, _: &StatusId) -> Option<&storyforge::combat_engine::StatusDef> {
            None
        }
        fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
            None
        }
    }
    let content = Multi(abilities);

    let mut scout = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos(0, 0)
        .hp(10, 10)
        .mp(2, 2)
        .regen(PoolKind::Mana, RegenRule::None)
        .regen(PoolKind::Energy, RegenRule::None)
        .build();
    scout.passives = vec![AbilityId::from("scout")];

    // Two hazards: id=10 at col=3 (distance 2 from arrival col=1) — should reveal;
    // id=11 at col=4 (distance 3 from arrival col=1) — must NOT reveal.
    let env = vec![
        EnvObject {
            id: EnvId(10),
            hex: hex_from_offset(3, 0),
            kind: EnvKind::Hazard,
            ability: AbilityId::from("trap"),
            owner: None,
            revealed_to: TeamSet::EMPTY,
        },
        EnvObject {
            id: EnvId(11),
            hex: hex_from_offset(4, 0),
            kind: EnvKind::Hazard,
            ability: AbilityId::from("trap"),
            owner: None,
            revealed_to: TeamSet::EMPTY,
        },
    ];

    let mut state = CombatState::new(vec![scout], 1, RoundPhase::ActorTurn, 0);
    state.environment = env;

    // Move one step to col=1.
    step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path: vec![hex_from_offset(1, 0)],
        },
        &mut storyforge::combat_engine::dice::ExpectedValue,
        &content,
    )
    .expect("move");

    let get = |id: u32| {
        state
            .environment
            .iter()
            .find(|e| e.id == EnvId(id))
            .unwrap()
            .visible_to(Team::Player)
    };
    assert!(get(10), "distance-2 hazard must be visible to player");
    assert!(!get(11), "distance-3 hazard must NOT be visible to player");
}

/// An ability with passive=[TurnStart,OnMove] fires BOTH at turn start
/// (stationary scan) and on move steps. Guards that both triggers share
/// the single `resolve_passives` handler.
#[test]
fn on_move_and_turn_start_share_handler() {
    use storyforge::combat_engine::content::aoe_radius;

    // Ability with BOTH triggers.
    let dual_def = AbilityDef {
        passive: vec![PassiveTrigger::TurnStart, PassiveTrigger::OnMove],
        aoe: AoEShape::Circle { radius: 2u32 },
        effect: EffectDef::RevealEnvInRange {
            range: aoe_radius(&AbilityDef {
                aoe: AoEShape::Circle { radius: 2u32 },
                ..AbilityDef::default()
            }),
        },
        target_type: TargetType::Environment,
        ..AbilityDef::default()
    };
    let content = Stub::new("dual", dual_def);

    let mut scout = unit(1, Team::Player, 0, 10, None);
    scout.passives = vec![AbilityId::from("dual")];

    let mut state = CombatState::new(vec![scout], 1, RoundPhase::ActorTurn, 0);
    // Hazard at col=1: within radius 2 of both start (col=0) and after move (col=0 still).
    state.environment = vec![hazard(5, 1)];

    // 1. TurnStart fires: hazard revealed.
    let ts_events = state.start_actor_turn(UnitId(1), &content);
    assert!(
        state.environment[0].visible_to(Team::Player),
        "TurnStart reveal must fire"
    );
    assert!(ts_events
        .iter()
        .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(5))));

    // 2. On a subsequent move step, already-revealed hazard emits nothing new (idempotent).
    let (_, ctx) = step(
        &mut state,
        Action::Move {
            actor: UnitId(1),
            path: vec![hex_from_offset(1, 0)],
        },
        &mut storyforge::combat_engine::dice::ExpectedValue,
        &content,
    )
    .expect("move step");
    // Already-revealed hazard emits no new EnvRevealed. The unit also steps onto
    // the hazard hex, so interrupt state is unrelated to the reveal — not asserted.
    let _ = ctx;

    // Reset and test OnMove alone (hazard not yet revealed, unit starts at col=0).
    let mut scout2 = unit(2, Team::Player, 0, 10, None);
    scout2.passives = vec![AbilityId::from("dual")];
    let mut state2 = CombatState::new(vec![scout2], 1, RoundPhase::ActorTurn, 0);
    // Hazard at col=3: within radius 2 of arrival at col=1.
    state2.environment = vec![hazard(9, 3)];

    let on_move_evs = state2.resolve_on_move_passives(UnitId(2), &content);
    // From col=0, hazard at col=3 is distance 3 — NOT in range.
    assert!(
        on_move_evs.is_empty(),
        "from start pos, col=3 hazard is out of radius 2"
    );

    // Manually move to col=1 so hazard at col=3 is distance 2.
    state2.unit_mut(UnitId(2)).unwrap().pos = hex_from_offset(1, 0);
    let on_move_evs2 = state2.resolve_on_move_passives(UnitId(2), &content);
    assert!(
        on_move_evs2
            .iter()
            .any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(9))),
        "OnMove trigger must reveal hazard now within radius 2"
    );
}

// ── T2: RevealEnv carries revealer team ─────────────────────────────────────

/// Reveal only inserts the caster's team into `revealed_to`; the opponent's
/// team is never granted visibility as a side effect.
#[test]
fn reveal_inserts_only_casters_team() {
    let content = Stub::new(
        "scout",
        AbilityDef {
            passive: vec![PassiveTrigger::TurnStart],
            effect: EffectDef::RevealEnvInRange { range: 5 },
            ..AbilityDef::default()
        },
    );
    let mut player = unit(1, Team::Player, 0, 10, None);
    player.passives = vec![AbilityId::from("scout")];
    let mut state = CombatState::new(vec![player], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(1, 1)];

    state.resolve_turn_start_passives(UnitId(1), &content);

    let e = &state.environment[0];
    assert!(
        e.visible_to(Team::Player),
        "player (caster) should see revealed trap"
    );
    assert!(
        !e.visible_to(Team::Enemy),
        "enemy must NOT see a player-revealed trap"
    );
    assert!(e.revealed_to.player, "player bit set");
    assert!(!e.revealed_to.enemy, "enemy bit must remain unset");
}

/// Revealing an owner-owned trap to the same team is a no-op; it must not
/// also reveal the trap to the opponent.
#[test]
fn owner_reveal_does_not_leak_to_opponent() {
    use storyforge::combat_engine::effect::{apply_effect, Effect};

    let player = unit(1, Team::Player, 0, 10, None);
    let no_content = crate::common::engine_unit::StubContent::new();
    let mut state = CombatState::new(vec![player], 1, RoundPhase::ActorTurn, 0);
    // Enemy-owned trap: enemy already sees it, player does not.
    state.environment = vec![EnvObject {
        id: EnvId(7),
        hex: hex_from_offset(1, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: Some(Team::Enemy),
        revealed_to: TeamSet::EMPTY,
    }];

    // Apply RevealEnv with revealer=Player (player team discovered it).
    let eff = Effect::RevealEnv {
        id: EnvId(7),
        revealer: Team::Player,
    };
    apply_effect(&mut state, &eff, &no_content);

    let e = &state.environment[0];
    // Enemy still sees it (via owner).
    assert!(
        e.visible_to(Team::Enemy),
        "enemy owner visibility preserved"
    );
    // Player now sees it (just revealed).
    assert!(e.visible_to(Team::Player), "player visibility after reveal");
    // Only player bit set — enemy bit untouched by the reveal.
    assert!(e.revealed_to.player, "player bit set in revealed_to");
    assert!(
        !e.revealed_to.enemy,
        "enemy bit NOT set in revealed_to (not needed: already owner)"
    );
}

/// Re-revealing the same trap to the same team is a no-op (no second EnvRevealed).
#[test]
fn reveal_idempotent_per_team() {
    let content = Stub::new(
        "scout",
        AbilityDef {
            passive: vec![PassiveTrigger::TurnStart],
            effect: EffectDef::RevealEnvInRange { range: 5 },
            ..AbilityDef::default()
        },
    );
    let mut player = unit(1, Team::Player, 0, 10, None);
    player.passives = vec![AbilityId::from("scout")];
    let mut state = CombatState::new(vec![player], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(3, 1)];

    let ev1 = state.resolve_turn_start_passives(UnitId(1), &content);
    assert_eq!(
        ev1.iter()
            .filter(|e| matches!(e, Event::EnvRevealed { .. }))
            .count(),
        1,
        "first reveal emits one EnvRevealed"
    );

    // Second call: already visible to player — no new event.
    let ev2 = state.resolve_turn_start_passives(UnitId(1), &content);
    assert!(
        ev2.iter().all(|e| !matches!(e, Event::EnvRevealed { .. })),
        "second reveal emits no EnvRevealed (idempotent)"
    );
}
