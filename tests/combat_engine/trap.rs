//! Commit B: environmental-trap trigger in the `step` pump loop.
//!
//! A trap is an `EnvObject { kind: Hazard }` in `CombatState.environment`.
//! Entering its hex during a `Move` fires the trap's `AbilityId` through the
//! Cast effect fanout, sourced by `EffectSource::Env`. Tests drive the public
//! `step(Action::Move)` (the trigger lives in `step_inner`).

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::Action,
    content::{AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, PassiveTrigger, StatusApplication, StatusOn, TargetType},
    dice::ExpectedValue,
    event::Event,
    state::{CombatState, EffectSource, EnvId, EnvKind, EnvObject, RoundPhase, Team, Unit, UnitId},
    step::step,
    AbilityId, DiceExpr, PoolKind, StatusBonuses, StatusDef, StatusId,
};
use storyforge::game::hex::hex_from_offset;

// ── Harness ─────────────────────────────────────────────────────────────────────

struct Stub(HashMap<AbilityId, AbilityDef>, StatusDef);
impl Stub {
    fn new(id: &str, def: AbilityDef) -> Self {
        Self(HashMap::from([(AbilityId::from(id), def)]), StatusDef {
            causes_disadvantage: false, blocks_mana_abilities: false, forces_targeting: false,
            skips_turn: false, hp_percent_dot: 0,
            bonuses: StatusBonuses { armor_bonus: 0, damage_taken_bonus: 0, speed_bonus: 0 },
        })
    }
}
impl ContentView for Stub {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> { self.0.get(id) }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { Some(&self.1) }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
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
        key: None, cost_ap: 0, costs: vec![], range: AbilityRange { min: 0, max: 0 },
        target_type: TargetType::SingleEnemy, aoe: AoEShape::None, friendly_fire: false,
        effect: EffectDef::Damage { dice: DiceExpr::new(n, 1, 0) },
        statuses: status.map(|s| vec![StatusApplication {
            status: StatusId::from(s), on: StatusOn::Target, duration_rounds: 2,
        }]).unwrap_or_default(),
        requires_los: false,
        passive: None,
    }
}

fn hazard(id: u32, col: i32) -> EnvObject {
    EnvObject { id: EnvId(id), hex: hex_from_offset(col, 0), kind: EnvKind::Hazard,
                ability: AbilityId::from("trap"), revealed: false }
}

/// Run a single-actor move over `cols` and return (state, events).
fn run(mover: Unit, env: Vec<EnvObject>, cols: &[i32], content: &Stub) -> (CombatState, Vec<Event>) {
    let mut state = CombatState::new(vec![mover], 1, RoundPhase::ActorTurn, 0);
    state.environment = env;
    let path = cols.iter().map(|&c| hex_from_offset(c, 0)).collect();
    let (events, _) = step(&mut state, Action::Move { actor: UnitId(1), path }, &mut ExpectedValue, content)
        .expect("move should succeed");
    (state, events)
}

fn hp(s: &CombatState, id: u64) -> i32 { s.unit(UnitId(id)).map(|u| u.hp()).unwrap_or(-1) }

// ── Tests ───────────────────────────────────────────────────────────────────────

/// Entering a hazard deals its damage, removes the trap (one-shot), and emits
/// HazardTriggered — for any team (symmetry). Firing is NOT a reveal.
#[test]
fn trap_triggers_damages_and_disappears() {
    for team in [Team::Player, Team::Enemy] {
        let (s, ev) = run(unit(1, team, 0, 10, None), vec![hazard(7, 1)], &[1],
                          &Stub::new("trap", trap_ability(2, None)));
        assert_eq!(hp(&s, 1), 8, "trap (2 dmg) for {team:?}");
        assert!(s.environment.is_empty(), "trap disappears after firing");
        assert!(ev.iter().any(|e| matches!(e, Event::HazardTriggered { victim, .. } if *victim == UnitId(1))));
        assert!(!ev.iter().any(|e| matches!(e, Event::EnvRevealed { .. })), "firing must not emit EnvRevealed");
    }
}

/// A trap on an intermediate hex fires on pass-through; the mover continues to
/// the final tile when the hit is non-lethal.
#[test]
fn trap_fires_on_pass_through() {
    let (s, _) = run(unit(1, Team::Player, 0, 10, None), vec![hazard(1, 1)], &[1, 2],
                     &Stub::new("trap", trap_ability(2, None)));
    assert_eq!(hp(&s, 1), 8);
    assert_eq!(s.unit(UnitId(1)).unwrap().pos, hex_from_offset(2, 0), "continues past a non-lethal trap");
}

/// A fired trap is removed; re-entering its hex does nothing.
#[test]
fn trap_gone_after_firing() {
    let content = Stub::new("trap", trap_ability(2, None));
    let mut state = CombatState::new(vec![unit(1, Team::Player, 0, 10, None)], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(1, 1)];
    // Pass through (1,0)->(2,0): fires + removes the trap.
    step(&mut state, Action::Move { actor: UnitId(1), path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)] },
         &mut ExpectedValue, &content).expect("first move");
    assert_eq!(hp(&state, 1), 8);
    assert!(state.environment.is_empty(), "trap removed after firing");
    // Step back onto the now-clear hex: no damage.
    step(&mut state, Action::Move { actor: UnitId(1), path: vec![hex_from_offset(1, 0)] },
         &mut ExpectedValue, &content).expect("second move");
    assert_eq!(hp(&state, 1), 8, "no trap remains to fire");
}

/// Env-sourced damage grants no rage to the source (no phantom unit/panic);
/// the victim still gains rage from being hurt.
#[test]
fn trap_no_rage_to_env_but_victim_gains() {
    let (s, _) = run(unit(1, Team::Player, 0, 10, Some((0, 10))), vec![hazard(1, 1)], &[1],
                     &Stub::new("trap", trap_ability(2, None)));
    let rage = s.unit(UnitId(1)).unwrap().pools[PoolKind::Rage].map(|(c, _)| c).unwrap();
    assert_eq!(rage, 1, "victim gains rage; env source grants none");
}

/// A lethal trap kills the mover on the trap hex and truncates the rest of the
/// path (existing dead-actor guard).
#[test]
fn trap_lethal_truncates_path() {
    let (s, ev) = run(unit(1, Team::Player, 0, 2, None), vec![hazard(1, 1)], &[1, 2],
                      &Stub::new("trap", trap_ability(2, None)));
    assert_eq!(hp(&s, 1), 0);
    assert_eq!(s.unit(UnitId(1)).unwrap().pos, hex_from_offset(1, 0), "dead mover doesn't advance past the trap");
    assert!(ev.iter().any(|e| matches!(e, Event::UnitDied { unit } if *unit == UnitId(1))));
}

/// A trap that applies a status uses an Env applier, not a phantom unit.
#[test]
fn trap_status_uses_env_applier() {
    let (s, _) = run(unit(1, Team::Player, 0, 10, None), vec![hazard(5, 1)], &[1],
                     &Stub::new("trap", trap_ability(1, Some("disoriented"))));
    let st = s.unit(UnitId(1)).unwrap().statuses.iter()
        .find(|s| s.id == StatusId::from("disoriented")).expect("status applied");
    assert_eq!(st.applier, EffectSource::Env(EnvId(5)));
}

/// `CombatState.environment` round-trips through serde (crate-level roundtrip
/// tests are dead, so cover the new field here).
#[test]
fn env_round_trips_through_serde() {
    let mut state = CombatState::new(vec![unit(1, Team::Player, 0, 10, None)], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(2, 1), hazard(1, 3)];
    let decoded: CombatState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    // Serialized sorted by id; both objects survive the round-trip.
    assert_eq!(decoded.environment.len(), 2);
    assert!(decoded.environment.iter().any(|e| e.id == EnvId(1) && e.hex == hex_from_offset(3, 0)));
    assert!(decoded.environment.iter().any(|e| e.id == EnvId(2)));
}

// ── Passive reveal (Kael "scout_traps") ─────────────────────────────────────────

/// A `RevealEnvInRange` passive with `TurnStart` reveals hidden hazards within
/// range only, and is idempotent (a second resolve emits no new EnvRevealed).
#[test]
fn passive_reveal_in_range_only_and_idempotent() {
    let scout = AbilityDef {
        passive: Some(PassiveTrigger::TurnStart),
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

    let revealed = |id: u32| state.environment.iter().find(|e| e.id == EnvId(id)).unwrap().revealed;
    assert!(revealed(10), "in-range hazard must be revealed");
    assert!(!revealed(20), "out-of-range hazard must stay hidden");
    assert_eq!(
        ev.iter().filter(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(10))).count(),
        1, "exactly one EnvRevealed for the in-range hazard",
    );
    assert!(!ev.iter().any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(20))));

    // Idempotent: re-firing reveals nothing new.
    let ev2 = state.resolve_turn_start_passives(UnitId(1), &content);
    assert!(ev2.is_empty(), "already-revealed hazards emit no new EnvRevealed");
}

/// A unit without the passive ability reveals nothing.
#[test]
fn no_passive_no_reveal() {
    let content = Stub::new("scout", AbilityDef {
        passive: Some(PassiveTrigger::TurnStart),
        effect: EffectDef::RevealEnvInRange { range: 5 },
        ..AbilityDef::default()
    });
    let u = unit(1, Team::Player, 0, 10, None); // passives empty by default
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(10, 1)];

    let ev = state.resolve_turn_start_passives(UnitId(1), &content);
    assert!(ev.is_empty(), "no passive → no events");
    assert!(!state.environment[0].revealed, "no passive → hazard stays hidden");
}

/// The passive fires through the real turn-start entry (`start_actor_turn`) and
/// consumes zero RNG (deterministic hex-distance scan).
#[test]
fn passive_reveal_via_start_actor_turn_zero_rng() {
    let content = Stub::new("scout", AbilityDef {
        passive: Some(PassiveTrigger::TurnStart),
        effect: EffectDef::RevealEnvInRange { range: 2 },
        ..AbilityDef::default()
    });
    let mut u = unit(1, Team::Player, 0, 10, None);
    u.passives = vec![AbilityId::from("scout")];
    let mut state = CombatState::new(vec![u], 1, RoundPhase::ActorTurn, 0);
    state.environment = vec![hazard(10, 1)];

    let events = state.start_actor_turn(UnitId(1), &content);
    assert!(state.environment[0].revealed, "start_actor_turn must fire the reveal passive");
    assert!(events.iter().any(|e| matches!(e, Event::EnvRevealed { env_id } if *env_id == EnvId(10))));
}
