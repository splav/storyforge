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
    caster_ctx: HashMap<UnitId, storyforge::combat_engine::CasterContext>,
}

impl StubContent {
    fn empty() -> Self {
        Self { abilities: HashMap::new(), caster_ctx: HashMap::new() }
    }

    fn with_ability(id: &str, def: AbilityDef) -> Self {
        let mut abilities = HashMap::new();
        abilities.insert(AbilityId::from(id), def);
        Self { abilities, caster_ctx: HashMap::new() }
    }

    fn with_caster(mut self, uid: UnitId, ctx: storyforge::combat_engine::CasterContext) -> Self {
        self.caster_ctx.insert(uid, ctx);
        self
    }
}

impl ContentView for StubContent {
    fn aoo_dice(&self, _: UnitId) -> Option<DiceExpr> { None }
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses { StatusBonuses::default() }
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> { self.abilities.get(id).cloned() }
    fn status_def(&self, _: &StatusId) -> Option<StatusDef> { None }
    fn caster_context(&self, actor: UnitId) -> storyforge::combat_engine::CasterContext {
        self.caster_ctx.get(&actor).cloned().unwrap_or_default()
    }
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

/// `step(Cast)` when the cast is fully legal and the ability has no damage effect →
/// `Ok` with exactly `[ActionStarted, ActionFinished]`.  Pins that a pure
/// status / None-effect ability with zero AP cost produces no state mutation.
#[test]
fn step_cast_returns_ok_when_legal() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // alive (hp=10)

    let mut state = state_with(vec![actor, target]);
    // cost_ap=0, EffectDef::None → no cost events, no damage events.
    let ability = AbilityDef {
        cost_ap: 0,
        costs: vec![],
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

// ── Step 6d tests: damage fanout ─────────────────────────────────────────────

/// Physical damage (Damage arm): raw = roll(1d6) + str_mod = 4 + 3 = 7.
/// Target starts at hp=10, ends at hp=3 (no armor).
#[test]
fn cast_damage_hits_target_with_str_mod() {
    use storyforge::combat_engine::CasterContext;

    let actor = make_unit(1, Team::Player, 0, 0); // action_points=2
    let target = make_unit(2, Team::Enemy, 1, 0);  // hp=10, armor=0

    let mut state = state_with(vec![actor, target]);

    // 1d6: expected = 1*(6+1)/2 = 3.5 → round = 4. raw = 4 + str_mod 3 = 7.
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability)
        .with_caster(UnitId(1), CasterContext { str_mod: 3, ..Default::default() });

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let events = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 3, "hp = 10 - (4+3) = 3");
    assert!(events.iter().any(|e| matches!(e, Event::UnitDamaged { target: UnitId(2), .. })));
}

/// Spell damage pierces armor: raw = roll(1d4) + int_mod + spell_power = 3+2+1 = 6.
/// Target has armor=10 but pierces=true → full 6 hp lost.
#[test]
fn cast_spell_damage_pierces_armor() {
    use storyforge::combat_engine::CasterContext;

    let actor = make_unit(1, Team::Player, 0, 0);
    let mut target = make_unit(2, Team::Enemy, 1, 0); // hp=10
    target.armor = 10;

    let mut state = state_with(vec![actor, target]);

    // 1d4: expected = 1*(4+1)/2 = 2.5 → round = 3. raw = 3 + int_mod 2 + spell_power 1 = 6.
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("firebolt", ability)
        .with_caster(UnitId(1), CasterContext { int_mod: 2, spell_power: 1, ..Default::default() });

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("firebolt"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // Armor ignored (pierces=true) → hp = 10 - 6 = 4.
    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 4, "piercing spell ignores armor=10");
}

/// WeaponAttack: raw = roll(1d8) + str_mod = 5 + 2 = 7. Target hp = 10-7 = 3.
#[test]
fn cast_weapon_attack_uses_weapon_dice() {
    use storyforge::combat_engine::CasterContext;

    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // hp=10, armor=0

    let mut state = state_with(vec![actor, target]);

    // 1d8: expected = 1*(8+1)/2 = 4.5 → round = 5. raw = 5 + str_mod 2 = 7.
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::WeaponAttack,
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("melee", ability)
        .with_caster(UnitId(1), CasterContext {
            str_mod: 2,
            weapon_dice: Some(DiceExpr::new(1, 8, 0)),
            ..Default::default()
        });

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("melee"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 3, "hp = 10 - (5+2) = 3");
}

/// WeaponAttack with no weapon_dice → no Damage effect, target hp unchanged.
#[test]
fn cast_weapon_attack_no_damage_when_no_weapon() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // hp=10

    let mut state = state_with(vec![actor, target]);

    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::WeaponAttack,
        ..single_enemy_ability()
    };
    // CasterContext::default() has weapon_dice=None.
    let content = StubContent::with_ability("unarmed", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("unarmed"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 10, "no weapon → no damage");
}

/// AoE Circle radius=1, fixed dice (0d1+2 → raw=2), str_mod=0.
/// Three enemies adjacent to target_pos all take 2 damage.
/// Per-target ordering: UnitDamaged events appear for each target without
/// interleaving from a later target's derived effects.
#[test]
fn cast_aoe_damages_targets_in_per_target_order() {
    use storyforge::combat_engine::{
        content::{TargetType as Tt},
        CasterContext,
    };

    let mut actor = make_unit(1, Team::Player, 0, 0);
    actor.action_points = 2;

    // Target center at (3,0); place 3 enemies: one at center, two at neighbors.
    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    // Use offset-coord helpers for unit positions; we need actual Hex values
    // to set unit.pos directly.
    let mut ea = make_unit(10, Team::Enemy, 0, 0);
    ea.pos = target_pos;
    ea.hp = 5;
    let mut eb = make_unit(11, Team::Enemy, 0, 0);
    eb.pos = neighbors[0];
    eb.hp = 5;
    let mut ec = make_unit(12, Team::Enemy, 0, 0);
    ec.pos = neighbors[1];
    ec.hp = 5;

    let mut state = state_with(vec![actor, ea, eb, ec]);

    // 0d1+2: expected = 0*(1+1)/2 + 2 = 2.0 → round = 2. str_mod=0 → raw=2.
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: Tt::Ground,
        aoe: AoEShape::Circle { radius: 1 },
        friendly_fire: false,
        effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 2) },
        statuses: vec![],
    };
    let content = StubContent::with_ability("fireball", ability)
        .with_caster(UnitId(1), CasterContext { str_mod: 0, ..Default::default() });

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        // For Ground targeting the primary target is actor (TargetType::Ground uses
        // actor_id in compute_affected_targets for non-AoE; AoE walks aoe_cells).
        target: UnitId(1),
        target_pos,
    };

    let events = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal AoE cast should succeed");

    // All three enemies take 2 damage: hp 5→3.
    assert_eq!(state.unit(UnitId(10)).unwrap().hp, 3, "EA hp = 5-2 = 3");
    assert_eq!(state.unit(UnitId(11)).unwrap().hp, 3, "EB hp = 5-2 = 3");
    assert_eq!(state.unit(UnitId(12)).unwrap().hp, 3, "EC hp = 5-2 = 3");

    // Per-target ordering: collect UnitDamaged positions and verify no
    // interleaving (each target's hit lands as a contiguous block relative
    // to the next target's hit — no UnitDamaged(B) before UnitDamaged(A)'s
    // derived effects).  With no rage on enemies, derived effects are just
    // Death (none here), so the stream must contain exactly 3 UnitDamaged
    // events and they must not repeat the same target_id.
    let damaged_ids: Vec<u64> = events.iter().filter_map(|e| {
        if let Event::UnitDamaged { target: UnitId(id), .. } = e { Some(*id) } else { None }
    }).collect();
    assert_eq!(damaged_ids.len(), 3, "exactly 3 UnitDamaged events");
    // All three targets appear.
    for id in [10u64, 11, 12] {
        assert!(damaged_ids.contains(&id), "UnitDamaged({id}) present");
    }
    // No duplicate: each target hit exactly once.
    let unique: std::collections::HashSet<_> = damaged_ids.iter().collect();
    assert_eq!(unique.len(), 3, "each target damaged exactly once");
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
