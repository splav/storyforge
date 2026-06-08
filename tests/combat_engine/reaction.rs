//! Step 5 unit tests: `scan_reactions` + `expand_reaction`.

use storyforge::combat_engine::{
    dice::{DiceExpr, ExpectedValue},
    reaction::{expand_reaction, scan_reactions, Reaction},
    state::{CombatState, EffectSource, RoundPhase, Team, Unit, UnitId},
};
use storyforge::game::hex::hex_from_offset;

use crate::common::engine_unit::{EngineUnitBuilder, StubContent};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// speed=4, Mp=4 — reaction defaults.
fn make_unit(id: u64, team: Team, reactions: i32) -> Unit {
    EngineUnitBuilder::new(id)
        .team(team)
        .speed(4)
        .mp(4, 4)
        .reactions(reactions, 1)
        .build()
}

fn state_with(units: Vec<Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::ActorTurn, 0)
}

// ── scan_reactions ────────────────────────────────────────────────────────────

/// Moving away from an adjacent enemy triggers one AoO.
#[test]
fn aoo_triggers_on_disengage() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0); // adjacent to mover
    let dest_pos = hex_from_offset(0, 2); // not adjacent to enemy

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;
    enemy.aoo_dice = Some(DiceExpr::new(1, 6, 0));

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::new();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);

    assert_eq!(reactions.len(), 1);
    assert!(matches!(
        reactions[0],
        Reaction::OpportunityAttack { from, victim }
        if from == UnitId(2) && victim == UnitId(1)
    ));
}

/// Enemy adjacent at both start and destination → no AoO (mover stays adjacent).
#[test]
fn aoo_does_not_fire_when_still_adjacent() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::new();

    // Find a neighbor of enemy_pos that is not mover_pos — still adjacent to the enemy.
    let dest_still_adj = enemy_pos
        .all_neighbors()
        .into_iter()
        .find(|&nb| nb != mover_pos)
        .unwrap();
    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_still_adj, &content);

    assert!(
        reactions.is_empty(),
        "no AoO when mover stays adjacent to enemy"
    );
}

/// Enemy has reactions_left == 0 → no AoO.
#[test]
fn aoo_does_not_fire_when_no_reactions() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos = hex_from_offset(0, 5); // far away

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 0); // no reactions
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::new();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(
        reactions.is_empty(),
        "no AoO when enemy has no reactions left"
    );
}

/// Enemy has no weapon → no AoO.
#[test]
fn aoo_does_not_fire_when_enemy_has_no_weapon() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos = hex_from_offset(0, 5);

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::new();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(reactions.is_empty(), "no AoO when enemy has no weapon");
}

/// Dead enemy cannot fire AoO.
#[test]
fn aoo_does_not_fire_from_dead_enemy() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos = hex_from_offset(0, 5);

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;
    enemy.pools[storyforge::combat_engine::PoolKind::Hp] = Some((0, 20)); // dead

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::new();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(reactions.is_empty(), "dead enemy cannot fire AoO");
}

// ── expand_reaction ───────────────────────────────────────────────────────────

/// expand_reaction emits DecrementReactions then Damage.
#[test]
fn expand_reaction_emits_decrement_then_damage() {
    use storyforge::combat_engine::effect::Effect;
    use storyforge::combat_engine::CasterContext;

    let reaction = Reaction::OpportunityAttack {
        from: UnitId(2),
        victim: UnitId(1),
    };
    let dice = DiceExpr::new(1, 6, 0);
    let content = StubContent::new();
    let mut rng = ExpectedValue;

    // Attacker (UnitId(2)) needs aoo_dice for expand_reaction's eligibility check.
    let mut attacker = make_unit(2, Team::Enemy, 1);
    attacker.aoo_dice = Some(dice);
    attacker.caster_context = CasterContext {
        weapon_dice: Some(dice),
        ..Default::default()
    };
    let victim = make_unit(1, Team::Player, 0);
    let state = state_with(vec![attacker, victim]);

    let effects = expand_reaction(&reaction, &state, &content, &mut rng);

    assert_eq!(effects.len(), 2);
    assert!(matches!(effects[0], Effect::DecrementReactions { actor } if actor == UnitId(2)));
    assert!(matches!(
        effects[1],
        Effect::Damage { target, source, pierces: false, .. }
        if target == UnitId(1) && source == EffectSource::Unit(UnitId(2))
    ));
}

/// Enemy mover, player attacker: symmetric to `aoo_triggers_on_disengage`.
///
/// Pins that `scan_reactions` has no hard-coded team bias — an enemy that
/// disengages from an adjacent armed player unit triggers the same AoO.
#[test]
fn aoo_triggers_when_enemy_disengages_from_player() {
    let mover_pos = hex_from_offset(0, 0);
    let attacker_pos = hex_from_offset(1, 0); // adjacent to mover
    let dest_pos = hex_from_offset(0, 5); // not adjacent to attacker

    let mut mover = make_unit(1, Team::Enemy, 4); // enemy moves
    mover.pos = mover_pos;
    let mut attacker = make_unit(2, Team::Player, 1); // player reacts
    attacker.pos = attacker_pos;
    attacker.aoo_dice = Some(DiceExpr::new(1, 6, 0));

    let state = state_with(vec![mover, attacker]);
    let content = StubContent::new();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);

    assert_eq!(reactions.len(), 1);
    assert!(matches!(
        reactions[0],
        Reaction::OpportunityAttack { from, victim }
        if from == UnitId(2) && victim == UnitId(1)
    ));
}

/// No weapon → expand_reaction returns empty.
#[test]
fn expand_reaction_returns_empty_when_no_weapon() {
    let reaction = Reaction::OpportunityAttack {
        from: UnitId(2),
        victim: UnitId(1),
    };
    let content = StubContent::new();
    let mut rng = ExpectedValue;

    // Attacker (UnitId(2)) has no weapon (caster_context.weapon_dice = None by default).
    let attacker = make_unit(2, Team::Enemy, 1);
    let victim = make_unit(1, Team::Player, 0);
    let state = state_with(vec![attacker, victim]);

    let effects = expand_reaction(&reaction, &state, &content, &mut rng);
    assert!(effects.is_empty());
}
