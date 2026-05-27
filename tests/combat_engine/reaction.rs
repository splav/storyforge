//! Step 5 unit tests: `scan_reactions` + `expand_reaction`.

use storyforge::combat_engine::{
    content::ContentView,
    dice::{DiceExpr, ExpectedValue},
    reaction::{expand_reaction, scan_reactions, Reaction},
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
};
use storyforge::combat_engine::StatusId;
use storyforge::game::hex::hex_from_offset;

// ── Helpers ───────────────────────────────────────────────────────────────────

#[allow(dead_code)]
struct StubContent {
    /// Previously used for ContentView::aoo_dice (removed in 5c.1).
    /// Kept for constructor compatibility; no longer read by the engine.
    aoo_dice: Option<DiceExpr>,
}

impl StubContent {
    fn with_weapon(d: DiceExpr) -> Self { Self { aoo_dice: Some(d) } }
    fn no_weapon() -> Self { Self { aoo_dice: None } }
}

impl ContentView for StubContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId) -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&storyforge::combat_engine::StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

fn make_unit(id: u64, team: Team, reactions: i32) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Unit {
        id: UnitId(id),
        team,
        pos: hex_from_offset(0, 0),
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 4,
        speed: 4,
        reactions_left: reactions,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((4, 4)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
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

// ── scan_reactions ────────────────────────────────────────────────────────────

/// Moving away from an adjacent enemy triggers one AoO.
#[test]
fn aoo_triggers_on_disengage() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0); // adjacent to mover
    let dest_pos  = hex_from_offset(0, 2); // not adjacent to enemy

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;
    enemy.aoo_dice = Some(DiceExpr::new(1, 6, 0));

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::with_weapon(DiceExpr::new(1, 6, 0));

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
    // Move to another neighbor of the enemy — still adjacent.
    let _dest_pos = hex_from_offset(0, 1); // unused; real dest computed below
    // We'll use a position that is definitely still adjacent (distance=1 to enemy).
    // Since exact adjacency depends on hex layout, we just test the rule:
    // if dest is still adjacent, no AoO.
    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::with_weapon(DiceExpr::new(1, 6, 0));

    // Find a neighbor of enemy_pos that is not mover_pos (so the mover
    // moves to a different adjacent hex — still adjacent to the enemy).
    let dest_still_adj = enemy_pos
        .all_neighbors()
        .into_iter()
        .find(|&nb| nb != mover_pos)
        .unwrap();
    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_still_adj, &content);

    assert!(reactions.is_empty(), "no AoO when mover stays adjacent to enemy");
}

/// Enemy has reactions_left == 0 → no AoO.
#[test]
fn aoo_does_not_fire_when_no_reactions() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos  = hex_from_offset(0, 5); // far away

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 0); // no reactions
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::with_weapon(DiceExpr::new(1, 6, 0));

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(reactions.is_empty(), "no AoO when enemy has no reactions left");
}

/// Enemy has no weapon → no AoO.
#[test]
fn aoo_does_not_fire_when_enemy_has_no_weapon() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos  = hex_from_offset(0, 5);

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::no_weapon();

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(reactions.is_empty(), "no AoO when enemy has no weapon");
}

/// Dead enemy cannot fire AoO.
#[test]
fn aoo_does_not_fire_from_dead_enemy() {
    let mover_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let dest_pos  = hex_from_offset(0, 5);

    let mut mover = make_unit(1, Team::Player, 1);
    mover.pos = mover_pos;
    let mut enemy = make_unit(2, Team::Enemy, 1);
    enemy.pos = enemy_pos;
    enemy.hp = 0; // dead

    let state = state_with(vec![mover, enemy]);
    let content = StubContent::with_weapon(DiceExpr::new(1, 6, 0));

    let reactions = scan_reactions(&state, UnitId(1), mover_pos, dest_pos, &content);
    assert!(reactions.is_empty(), "dead enemy cannot fire AoO");
}

// ── expand_reaction ───────────────────────────────────────────────────────────

/// expand_reaction emits DecrementReactions then Damage.
#[test]
fn expand_reaction_emits_decrement_then_damage() {
    use storyforge::combat_engine::effect::Effect;
    use storyforge::combat_engine::CasterContext;

    let reaction = Reaction::OpportunityAttack { from: UnitId(2), victim: UnitId(1) };
    let dice = DiceExpr::new(1, 6, 0);
    let content = StubContent::with_weapon(dice);
    let mut rng = ExpectedValue;

    // Attacker (UnitId(2)) needs aoo_dice for expand_reaction's eligibility check.
    let mut attacker = make_unit(2, Team::Enemy, 1);
    attacker.aoo_dice = Some(dice);
    attacker.caster_context = CasterContext { weapon_dice: Some(dice), ..Default::default() };
    let victim = make_unit(1, Team::Player, 0);
    let state = state_with(vec![attacker, victim]);

    let effects = expand_reaction(&reaction, &state, &content, &mut rng);

    assert_eq!(effects.len(), 2);
    assert!(matches!(effects[0], Effect::DecrementReactions { actor } if actor == UnitId(2)));
    assert!(matches!(
        effects[1],
        Effect::Damage { target, source, pierces: false, .. }
        if target == UnitId(1) && source == UnitId(2)
    ));
}

/// Enemy mover, player attacker: symmetric to `aoo_triggers_on_disengage`.
///
/// Pins that `scan_reactions` has no hard-coded team bias — an enemy that
/// disengages from an adjacent armed player unit triggers the same AoO.
#[test]
fn aoo_triggers_when_enemy_disengages_from_player() {
    let mover_pos  = hex_from_offset(0, 0);
    let attacker_pos = hex_from_offset(1, 0); // adjacent to mover
    let dest_pos   = hex_from_offset(0, 5);   // not adjacent to attacker

    let mut mover = make_unit(1, Team::Enemy, 4);  // enemy moves
    mover.pos = mover_pos;
    let mut attacker = make_unit(2, Team::Player, 1); // player reacts
    attacker.pos = attacker_pos;
    attacker.aoo_dice = Some(DiceExpr::new(1, 6, 0));

    let state = state_with(vec![mover, attacker]);
    let content = StubContent::with_weapon(DiceExpr::new(1, 6, 0));

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
    let reaction = Reaction::OpportunityAttack { from: UnitId(2), victim: UnitId(1) };
    let content = StubContent::no_weapon();
    let mut rng = ExpectedValue;

    // Attacker (UnitId(2)) has no weapon (caster_context.weapon_dice = None by default).
    let attacker = make_unit(2, Team::Enemy, 1);
    let victim = make_unit(1, Team::Player, 0);
    let state = state_with(vec![attacker, victim]);

    let effects = expand_reaction(&reaction, &state, &content, &mut rng);
    assert!(effects.is_empty());
}
