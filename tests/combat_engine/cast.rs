//! Step 6b tests: `step(Action::Cast)` — legality pre-validate only.
//!
//! Effect fanout (PayCost, Damage, Heal, ApplyStatus) belongs to steps 6c-f.
//! These tests pin the three legality paths:
//! - unknown ability → `Illegal(UnknownAbility)`
//! - dead target → `Illegal(TargetDead)`
//! - legal cast → `Ok([ActionStarted, ActionFinished])`, state unchanged.

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, StatusBonuses, TargetType,
    },
    dice::{DiceExpr, ExpectedValue},
    event::Event,
    legality::IllegalReason,
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    AbilityId, StatusId,
};
use storyforge::combat_engine::StatusDef;
use storyforge::game::hex::hex_from_offset;

// ── StubContent ───────────────────────────────────────────────────────────────

struct StubContent {
    abilities: HashMap<AbilityId, AbilityDef>,
}

impl StubContent {
    fn empty() -> Self {
        Self { abilities: HashMap::new() }
    }

    fn with_ability(id: &str, def: AbilityDef) -> Self {
        let mut abilities = HashMap::new();
        abilities.insert(AbilityId::from(id), def);
        Self { abilities }
    }
}

impl ContentView for StubContent {
    fn aoo_dice(&self, _: UnitId) -> Option<DiceExpr> { None }
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses { StatusBonuses::default() }
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> { self.abilities.get(id).cloned() }
    fn status_def(&self, _: &StatusId) -> Option<StatusDef> { None }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_unit(id: u64, team: Team, pos_col: i32, pos_row: i32) -> Unit {
    Unit {
        id: UnitId(id),
        team,
        pos: hex_from_offset(pos_col, pos_row),
        hp: 10,
        max_hp: 10,
        armor: 0,
        armor_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 2,
        movement_points: 6,
        reactions_left: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
    }
}

fn state_with(units: Vec<Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::ActorTurn, 0)
}

/// A minimal `AbilityDef` for a `SingleEnemy` melee spell.
fn single_enemy_ability() -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        statuses: vec![],
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `step(Cast)` with an ability id not in content → `Illegal(UnknownAbility)`.
#[test]
fn step_cast_returns_err_illegal_for_unknown_ability() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);
    let mut state = state_with(vec![actor, target]);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("nope"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let err = step(&mut state, action, &mut ExpectedValue, &StubContent::empty())
        .expect_err("unknown ability should be rejected");

    assert_eq!(err, ActionError::Illegal(IllegalReason::UnknownAbility));
    // State must be unchanged (rollback).
    assert_eq!(state.unit(UnitId(1)).unwrap().action_points, 2);
}

/// `step(Cast)` targeting a dead unit (hp=0) → `Illegal(TargetDead)`.
#[test]
fn step_cast_returns_err_illegal_for_dead_target() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let mut target = make_unit(2, Team::Enemy, 1, 0);
    target.hp = 0; // corpse

    let mut state = state_with(vec![actor, target]);
    let content = StubContent::with_ability("fireball", single_enemy_ability());

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let err = step(&mut state, action, &mut ExpectedValue, &content)
        .expect_err("dead target should be rejected");

    assert_eq!(err, ActionError::Illegal(IllegalReason::TargetDead));
    // State must be unchanged (rollback).
    assert_eq!(state.unit(UnitId(1)).unwrap().action_points, 2);
}

/// `step(Cast)` when the cast is fully legal → `Ok` with exactly
/// `[ActionStarted, ActionFinished]`. No state mutation (no effects yet —
/// this test is preserved from step 6b; the ability has cost_ap=0 so no
/// AP is spent and state stays unchanged).
#[test]
fn step_cast_returns_ok_when_legal() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // alive (hp=10)

    let mut state = state_with(vec![actor, target]);
    // cost_ap=0 so no DecrementAP fires.
    let ability = AbilityDef {
        cost_ap: 0,
        costs: vec![],
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("fireball", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let events = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // Exactly ActionStarted + ActionFinished — no effect events.
    assert_eq!(events.len(), 2, "expected exactly [ActionStarted, ActionFinished]");
    assert!(matches!(events[0], Event::ActionStarted { .. }));
    assert!(matches!(events[1], Event::ActionFinished { .. }));

    // State is unchanged — no AP spent, no HP lost.
    assert_eq!(state.unit(UnitId(1)).unwrap().action_points, 2, "actor AP unchanged");
    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 10, "target HP unchanged");
}

// ── Step 6c tests: cost payment ───────────────────────────────────────────────

/// AP cost is deducted after a legal cast.
#[test]
fn cast_legal_pays_ap_cost() {
    let actor = make_unit(1, Team::Player, 0, 0); // action_points=2
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::None,
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("zap", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("zap"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let events = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(1)).unwrap().action_points, 1, "AP decremented by 1");
    assert!(matches!(events.last(), Some(Event::ActionFinished { .. })));
}

/// Mana cost (and AP cost) are both paid after a legal cast.
#[test]
fn cast_legal_pays_mana_cost() {
    use storyforge::combat_engine::content::Cost;
    use storyforge::combat_engine::ResourceKind;

    let mut actor = make_unit(1, Team::Player, 0, 0); // action_points=2
    actor.mana = Some((10, 10));
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![Cost { resource: ResourceKind::Mana, amount: 3 }],
        effect: EffectDef::None,
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("fireball", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    let u = state.unit(UnitId(1)).unwrap();
    assert_eq!(u.action_points, 1, "AP decremented by 1");
    assert_eq!(u.mana, Some((7, 10)), "mana decremented by 3");
}

/// Zero-cost ability emits only `[ActionStarted, ActionFinished]` with no state change.
#[test]
fn cast_legal_pays_no_cost_when_zero() {
    let actor = make_unit(1, Team::Player, 0, 0); // action_points=2
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 0,
        costs: vec![],
        effect: EffectDef::None,
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("meditate", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("meditate"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let events = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // No DecrementAP or PayCost fired — both produce no event — so only
    // ActionStarted + ActionFinished should be present.
    assert_eq!(events.len(), 2, "only [ActionStarted, ActionFinished]");
    assert!(matches!(events[0], Event::ActionStarted { .. }));
    assert!(matches!(events[1], Event::ActionFinished { .. }));

    assert_eq!(state.unit(UnitId(1)).unwrap().action_points, 2, "AP unchanged for zero-cost");
}
