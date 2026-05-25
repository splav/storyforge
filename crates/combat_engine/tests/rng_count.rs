/// RNG call-count canary tests (Phase 5 D2).
///
/// Verifies that `ApplyCtx::rng_calls` accurately reflects the number of
/// `DiceSource::roll_d` invocations consumed by each `step()` call, and that
/// `ExpectedValue::call_count()` stays at 0 (deterministic source).

use combat_engine::{
    AbilityId,
    UnitTemplate,
    action::Action,
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, CritFailOutcome, EffectDef,
        StatusBonuses, TargetType,
    },
    dice::{DiceExpr, DiceRng, DiceSource, ExpectedValue},
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    StatusDef, StatusId,
};
use hexx::Hex;

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_unit(id: u64, team: Team, pos: Hex) -> Unit {
    Unit {
        id: UnitId(id),
        team,
        pos,
        hp: 20,
        max_hp: 20,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 6,
        speed: 6,
        action_points: 4,
        max_ap: 4,
        movement_points: 10,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    }
}

// ── ContentView stubs ─────────────────────────────────────────────────────────

/// No weapon, no abilities.
struct NoWeaponContent;

impl ContentView for NoWeaponContent {
    fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<UnitTemplate> { None }
}

/// Minimal content stub for AoO tests (weapon dice now on unit.caster_context).
#[allow(dead_code)]
struct WithWeaponContent(DiceExpr);

/// Weapon dice now live on Unit.caster_context.weapon_dice (5c.1).
/// This impl provides only the 4 static-content methods.
impl ContentView for WithWeaponContent {
    fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<UnitTemplate> { None }
}

/// Single ability definition, crit-fail = Miss.
struct CastContent {
    id: String,
    def: AbilityDef,
}

impl ContentView for CastContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        if id.0 == self.id { Some(&self.def) } else { None }
    }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<UnitTemplate> { None }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// `ExpectedValue.call_count()` stays at 0 even after many `roll()` invocations.
/// Deterministic source has no RNG stream to advance.
#[test]
fn expected_value_call_count_always_zero() {
    let mut ev = ExpectedValue;
    let _ = ev.roll(DiceExpr::new(2, 6, 3));
    let _ = ev.roll(DiceExpr::new(1, 20, 0));
    let _ = ev.roll_disadvantage(DiceExpr::new(1, 8, 0));
    assert_eq!(ev.call_count(), 0, "ExpectedValue.call_count() must always be 0");
}

/// `Action::Move` with no enemies in range consumes 0 RNG rolls.
#[test]
fn move_no_aoo_consumes_zero_rolls() {
    let actor = make_unit(1, Team::Player, Hex::ZERO);
    let mut state = CombatState::new(vec![actor], 1, RoundPhase::ActorTurn, 0);
    let mut rng = DiceRng::with_seed(0xDEAD);

    let path = vec![Hex::new(1, 0), Hex::new(2, 0)];
    let (_, ctx) = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut rng,
        &NoWeaponContent,
    )
    .expect("move should succeed");

    assert_eq!(ctx.rng_calls, 0, "Move with no AoO must consume 0 RNG rolls");
}

/// `Action::Move` provoking 1 AoO consumes exactly 1 RNG roll.
///
/// `expand_reaction` calls `rng.roll(weapon_dice)`. With weapon `DiceExpr { count: 1, .. }`,
/// `roll_d` is called exactly once per AoO.
#[test]
fn move_with_one_aoo_consumes_one_roll() {
    // Actor at ZERO, enemy at (1,0). Actor moves to (-1,0) to disengage.
    let mut actor = make_unit(1, Team::Player, Hex::ZERO);
    actor.hp = 20;

    let enemy_pos = Hex::new(1, 0);
    let mut enemy = make_unit(2, Team::Enemy, enemy_pos);
    enemy.reactions_left = 1;

    // Destination: move away so actor leaves enemy's ZoC.
    // (-1,0) is distance 1 from ZERO; verify it is NOT adjacent to enemy at (1,0).
    let dest = Hex::new(-1, 0);
    assert_eq!(Hex::ZERO.unsigned_distance_to(enemy_pos), 1, "enemy adjacent to start");
    assert_ne!(dest.unsigned_distance_to(enemy_pos), 1, "dest must not be adjacent to enemy");

    // Weapon: 1d6 (count=1 → exactly 1 roll_d call). AoO dice lives on unit.aoo_dice (5c.1).
    let weapon = DiceExpr::new(1, 6, 0);
    enemy.aoo_dice = Some(weapon);

    let mut state = CombatState::new(vec![actor, enemy], 1, RoundPhase::ActorTurn, 0);
    let mut rng = DiceRng::with_seed(0xCAFE);
    rng.script(&[1]); // AoO roll = 1

    let (_, ctx) = step(
        &mut state,
        Action::Move { actor: UnitId(1), path: vec![dest] },
        &mut rng,
        &WithWeaponContent(weapon),
    )
    .expect("move with AoO should succeed");

    assert_eq!(ctx.rng_calls, 1, "Move with 1 AoO (1d6 weapon) must consume exactly 1 roll");
}

/// `Action::Cast` on 3 targets consumes 1 (d20 crit-fail) + 3 (damage, 1d4 each) = 4 rolls.
#[test]
fn cast_3_targets_consumes_d20_plus_3_damage_rolls() {
    // Actor + 3 enemies clustered around target_pos.
    let target_pos = Hex::new(2, 0);
    let neighbors: Vec<Hex> = target_pos.all_neighbors().to_vec();

    let mut actor = make_unit(1, Team::Player, Hex::ZERO);
    actor.action_points = 4;
    // CritFailOutcome is now on the unit (5c.1); CastContent only needs ability_def.
    actor.caster_context.crit_fail_outcome = CritFailOutcome::Miss;

    let ea = make_unit(10, Team::Enemy, target_pos);
    let eb = make_unit(11, Team::Enemy, neighbors[0]);
    let ec = make_unit(12, Team::Enemy, neighbors[1]);

    let mut state = CombatState::new(vec![actor, ea, eb, ec], 1, RoundPhase::ActorTurn, 0);

    // AoE Circle radius=1, 1d4 damage per target → 1 roll_d per target.
    let def = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 8 },
        target_type: TargetType::Ground,
        aoe: AoEShape::Circle { radius: 1 },
        friendly_fire: false,
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        statuses: vec![],
    };
    let content = CastContent { id: "fireball".to_string(), def };

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(1), // Ground targeting: actor used as primary id
        target_pos,
    };

    // Script: d20=11 (no crit-fail), then 3 damage rolls of 1 each.
    let mut rng = DiceRng::with_seed(0xBEEF);
    rng.script(&[11, 1, 1, 1]);

    let (_, ctx) = step(&mut state, action, &mut rng, &content).expect("cast should succeed");

    assert_eq!(
        ctx.rng_calls, 4,
        "Cast on 3 targets (1d4 damage): 1 (d20) + 3 (damage) = 4 rolls; got {}",
        ctx.rng_calls
    );
}
