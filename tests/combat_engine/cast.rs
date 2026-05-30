//! Step 6b-6e tests: `step(Action::Cast)`.
//!
//! Covers:
//! - Step 6b: legality pre-validate (unknown ability, dead target, legal cast).
//! - Step 6c: AP + resource cost payment.
//! - Step 6d: damage fanout (Damage, SpellDamage, WeaponAttack, AoE).
//! - Step 6e: Heal fanout + status application (Target / MySelf / AoE).
//! - Step 3.5b: Summon → Effect::Spawn → UnitSpawned / SpawnBlocked.

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::{
        AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, StatusApplication,
        StatusOn, TargetType, UnitTemplate,
    },
    dice::{DiceExpr, DiceRng, ExpectedValue},
    effect::SpawnBlockedReason,
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
    templates: HashMap<String, storyforge::combat_engine::UnitTemplate>,
}

impl StubContent {
    fn empty() -> Self {
        Self { abilities: HashMap::new(), caster_ctx: HashMap::new(), templates: HashMap::new() }
    }

    fn with_ability(id: &str, def: AbilityDef) -> Self {
        let mut abilities = HashMap::new();
        abilities.insert(AbilityId::from(id), def);
        Self { abilities, caster_ctx: HashMap::new(), templates: HashMap::new() }
    }

    /// Record a per-unit CasterContext to be applied to the CombatState.
    ///
    /// Call `apply_caster_contexts(&mut state)` after constructing the state.
    fn with_caster(mut self, uid: UnitId, ctx: storyforge::combat_engine::CasterContext) -> Self {
        self.caster_ctx.insert(uid, ctx);
        self
    }

    /// Apply stored CasterContexts to unit fields.
    ///
    /// Must be called after `state_with(...)` because CasterContext now lives on
    /// `Unit.caster_context` (5c.1), not on `ContentView`.
    fn apply_caster_contexts(&self, state: &mut storyforge::combat_engine::state::CombatState) {
        for (&uid, ctx) in &self.caster_ctx {
            if let Some(u) = state.unit_mut(uid) {
                u.caster_context = ctx.clone();
            }
        }
    }

    fn with_template(mut self, id: &str, tmpl: storyforge::combat_engine::UnitTemplate) -> Self {
        self.templates.insert(id.to_string(), tmpl);
        self
    }
}

impl ContentView for StubContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> { self.abilities.get(id) }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, id: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
        self.templates.get(id).cloned()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_unit(id: u64, team: Team, pos_col: i32, pos_row: i32) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Unit::new(
        UnitId(id),
        team,
        hex_from_offset(pos_col, pos_row),
        0,
        0,
        0,
        6,
        6,
        1,
        1,
        vec![],
        None,
        Default::default(),
        None,
        Vec::new(),
        Vec::new(),
        storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => Some((10, 10)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((6, 6)),
        },
        storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        None,
    )
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
        requires_los: false,
        passive: vec![],
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
        .map(|(ev, _)| ev)
        .expect_err("unknown ability should be rejected");

    assert_eq!(err, ActionError::Illegal(IllegalReason::UnknownAbility));
    // State must be unchanged (rollback).
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 2);
}

/// `step(Cast)` targeting a dead unit (hp=0) → `Illegal(TargetDead)`.
#[test]
fn step_cast_returns_err_illegal_for_dead_target() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let mut target = make_unit(2, Team::Enemy, 1, 0);
    target.pools[combat_engine::PoolKind::Hp] = Some((0, 10));

    let mut state = state_with(vec![actor, target]);
    let content = StubContent::with_ability("fireball", single_enemy_ability());

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let err = step(&mut state, action, &mut ExpectedValue, &content)
        .map(|(ev, _)| ev)
        .expect_err("dead target should be rejected");

    assert_eq!(err, ActionError::Illegal(IllegalReason::TargetDead));
    // State must be unchanged (rollback).
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 2);
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

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // Exactly ActionStarted + ActionFinished — no effect events.
    assert_eq!(events.len(), 2, "expected exactly [ActionStarted, ActionFinished]");
    assert!(matches!(events[0], Event::ActionStarted { .. }));
    assert!(matches!(events[1], Event::ActionFinished { .. }));

    // State is unchanged — no AP spent, no HP lost.
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 2, "actor AP unchanged");
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 10, "target HP unchanged");
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

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP decremented by 1");
    assert!(matches!(events.last(), Some(Event::ActionFinished { .. })));
}

/// Mana cost (and AP cost) are both paid after a legal cast.
#[test]
fn cast_legal_pays_mana_cost() {
    use storyforge::combat_engine::content::Cost;
    use storyforge::combat_engine::ResourceKind;

    let mut actor = make_unit(1, Team::Player, 0, 0); // action_points=2
    actor.pools[storyforge::combat_engine::PoolKind::Mana] = Some((10, 10));
    actor.pools[storyforge::combat_engine::PoolKind::Mana] = Some((10, 10));
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
    assert_eq!(u.pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP decremented by 1");
    assert_eq!(u.pools[storyforge::combat_engine::PoolKind::Mana], Some((7, 10)), "mana decremented by 3");
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
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 3, "hp = 10 - (4+3) = 3");
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
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("firebolt"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // Armor ignored (pierces=true) → hp = 10 - 6 = 4.
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 4, "piercing spell ignores armor=10");
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
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("melee"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 3, "hp = 10 - (5+2) = 3");
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

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 10, "no weapon → no damage");
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

    let actor = make_unit(1, Team::Player, 0, 0);
    // AP is already 2 from make_unit; pool is canonical since Phase C-6.

    // Target center at (3,0); place 3 enemies: one at center, two at neighbors.
    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    // Use offset-coord helpers for unit positions; we need actual Hex values
    // to set unit.pos directly.
    let mut ea = make_unit(10, Team::Enemy, 0, 0);
    ea.pos = target_pos;
    ea.pools[combat_engine::PoolKind::Hp] = Some((5, 10));
    let mut eb = make_unit(11, Team::Enemy, 0, 0);
    eb.pos = neighbors[0];
    eb.pools[combat_engine::PoolKind::Hp] = Some((5, 10));
    let mut ec = make_unit(12, Team::Enemy, 0, 0);
    ec.pos = neighbors[1];
    ec.pools[combat_engine::PoolKind::Hp] = Some((5, 10));

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
        requires_los: false,
        passive: vec![],
    };
    let content = StubContent::with_ability("fireball", ability)
        .with_caster(UnitId(1), CasterContext { str_mod: 0, ..Default::default() });
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        // For Ground targeting the primary target is actor (TargetType::Ground uses
        // actor_id in compute_affected_targets for non-AoE; AoE walks aoe_cells).
        target: UnitId(1),
        target_pos,
    };

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal AoE cast should succeed");

    // All three enemies take 2 damage: hp 5→3.
    assert_eq!(state.unit(UnitId(10)).unwrap().hp(), 3, "EA hp = 5-2 = 3");
    assert_eq!(state.unit(UnitId(11)).unwrap().hp(), 3, "EB hp = 5-2 = 3");
    assert_eq!(state.unit(UnitId(12)).unwrap().hp(), 3, "EC hp = 5-2 = 3");

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

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // No DecrementAP or PayCost fired — both produce no event — so only
    // ActionStarted + ActionFinished should be present.
    assert_eq!(events.len(), 2, "only [ActionStarted, ActionFinished]");
    assert!(matches!(events[0], Event::ActionStarted { .. }));
    assert!(matches!(events[1], Event::ActionFinished { .. }));

    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 2, "AP unchanged for zero-cost");
}

// ── Step 6e tests: heal + status fanout ──────────────────────────────────────

/// Heal: amount = roll(1d4) + int_mod + spell_power = 3 + 2 + 1 = 6.
/// Target starts at hp=3/max_hp=10, ends at hp=9.
#[test]
fn cast_heal_restores_target_hp() {
    use storyforge::combat_engine::CasterContext;

    let actor = make_unit(1, Team::Player, 0, 0);
    let mut target = make_unit(2, Team::Player, 1, 0);
    target.pools[combat_engine::PoolKind::Hp] = Some((3, 10));

    let mut state = state_with(vec![actor, target]);

    // 1d4: expected = 1*(4+1)/2 = 2.5 → round = 3. amount = 3 + 2 + 1 = 6.
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleAlly,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Heal { dice: DiceExpr::new(1, 4, 0) },
        statuses: vec![],
        requires_los: false,
        passive: vec![],
    };
    let content = StubContent::with_ability("heal", ability)
        .with_caster(UnitId(1), CasterContext { int_mod: 2, spell_power: 1, ..Default::default() });
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("heal"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal heal cast should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 9, "hp = 3 + (3+2+1) = 9");
}

/// AoE heal (fixed dice 0d1+4 = 4, int_mod=0, spell_power=0).
/// Three allies within radius 1 of target_pos all restored from hp=3 to hp=7.
#[test]
fn cast_aoe_heal_restores_multiple_targets() {
    let actor_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    let mut actor = make_unit(1, Team::Player, 0, 0);
    actor.pos = actor_pos;

    let mut a1 = make_unit(10, Team::Player, 0, 0);
    a1.pos = target_pos;
    a1.pools[combat_engine::PoolKind::Hp] = Some((3, 10));
    let mut a2 = make_unit(11, Team::Player, 0, 0);
    a2.pos = neighbors[0];
    a2.pools[combat_engine::PoolKind::Hp] = Some((3, 10));
    let mut a3 = make_unit(12, Team::Player, 0, 0);
    a3.pos = neighbors[1];
    a3.pools[combat_engine::PoolKind::Hp] = Some((3, 10));

    let mut state = state_with(vec![actor, a1, a2, a3]);

    // 0d1+4: expected = 0*(1+1)/2 + 4 = 4.0 → round = 4. int_mod=0, spell_power=0.
    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 8 },
        target_type: TargetType::Ground,
        aoe: AoEShape::Circle { radius: 1 },
        friendly_fire: true, // include all units in AoE (allies)
        effect: EffectDef::Heal { dice: DiceExpr::new(0, 1, 4) },
        statuses: vec![],
        requires_los: false,
        passive: vec![],
    };
    let content = StubContent::with_ability("group_heal", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("group_heal"),
        target: UnitId(1), // Ground targeting uses actor as primary target id
        target_pos,
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal AoE heal should succeed");

    assert_eq!(state.unit(UnitId(10)).unwrap().hp(), 7, "a1 hp = 3 + 4 = 7");
    assert_eq!(state.unit(UnitId(11)).unwrap().hp(), 7, "a2 hp = 3 + 4 = 7");
    assert_eq!(state.unit(UnitId(12)).unwrap().hp(), 7, "a3 hp = 3 + 4 = 7");
}

/// Status with `on: Target` is applied to the targeted enemy.
/// dot_per_tick = 0 (Phase 2 limitation).
#[test]
fn cast_applies_status_to_target() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);

    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::None,
        statuses: vec![StatusApplication {
            status: StatusId::from("poison"),
            duration_rounds: 3,
            on: StatusOn::Target,
        }],
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("poison_strike", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("poison_strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    let target_unit = state.unit(UnitId(2)).unwrap();
    assert_eq!(target_unit.statuses.len(), 1, "exactly one status applied");
    let s = &target_unit.statuses[0];
    assert_eq!(s.id, StatusId::from("poison"));
    assert_eq!(s.rounds_remaining, 3);
    assert_eq!(s.dot_per_tick, 0, "Phase 2: dot_per_tick always 0");
    assert_eq!(s.applier, combat_engine::state::EffectSource::Unit(UnitId(1)));
}

/// Status with `on: MySelf` lands on the actor, not the targeted unit.
/// Uses TargetType::Myself so the proposed target == actor.
#[test]
fn cast_applies_status_to_self_via_myself() {
    let mut actor = make_unit(1, Team::Player, 0, 0);
    // TargetType::Myself: target must be the actor; place both at same cell.
    let caster_pos = hex_from_offset(2, 2);
    actor.pos = caster_pos;

    let mut state = state_with(vec![actor]);

    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 0 },
        target_type: TargetType::Myself,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::None,
        statuses: vec![StatusApplication {
            status: StatusId::from("iron_skin"),
            duration_rounds: 2,
            on: StatusOn::MySelf,
        }],
        requires_los: false,
        passive: vec![],
    };
    let content = StubContent::with_ability("iron_skin", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("iron_skin"),
        target: UnitId(1), // Myself: target == actor
        target_pos: caster_pos,
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("self-buff should succeed");

    let caster = state.unit(UnitId(1)).unwrap();
    assert_eq!(caster.statuses.len(), 1, "actor has one status");
    let s = &caster.statuses[0];
    assert_eq!(s.id, StatusId::from("iron_skin"));
    assert_eq!(s.rounds_remaining, 2);
    assert_eq!(s.applier, combat_engine::state::EffectSource::Unit(UnitId(1)), "applier = caster");
}

// ── Step 6f tests: crit-fail ─────────────────────────────────────────────────

/// d20 = 1 + Miss outcome → no damage to target; AP still paid.
#[test]
fn cast_crit_fail_miss_skips_damage() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0); // hp=10, ap=2
    let target = make_unit(2, Team::Enemy, 1, 0);  // hp=10

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext { crit_fail_outcome: CritFailOutcome::Miss, ..Default::default() },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // d20=1 → crit-fail; no damage roll follows.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]);

    step(&mut state, action, &mut rng, &content).expect("should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 10, "target hp unchanged on miss crit-fail");
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP still paid");
}

/// d20 = 1 + DoubleCost outcome → mana cost doubled; no damage to target.
#[test]
fn cast_crit_fail_double_cost() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};
    use storyforge::combat_engine::content::Cost;
    use storyforge::combat_engine::ResourceKind;

    let mut actor = make_unit(1, Team::Player, 0, 0);
    actor.pools[storyforge::combat_engine::PoolKind::Mana] = Some((10, 10));
    actor.pools[storyforge::combat_engine::PoolKind::Mana] = Some((10, 10));
    let target = make_unit(2, Team::Enemy, 1, 0); // hp=10

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![Cost { resource: ResourceKind::Mana, amount: 3 }],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("fireball", ability).with_caster(
        UnitId(1),
        CasterContext { crit_fail_outcome: CritFailOutcome::DoubleCost, ..Default::default() },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("fireball"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // d20=1 → crit-fail DoubleCost; no damage roll.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]);

    step(&mut state, action, &mut rng, &content).expect("should succeed");

    let actor_unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(actor_unit.pools[storyforge::combat_engine::PoolKind::Mana], Some((4, 10)), "mana = 10 - 3*2 = 4");
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 10, "target hp unchanged");
}

/// d20 = 1 + SelfDamage(0d1+3) → caster takes 3 raw damage; target unaffected.
#[test]
fn cast_crit_fail_self_damage() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0); // hp=10, armor=0
    let target = make_unit(2, Team::Enemy, 1, 0);  // hp=10

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    // SelfDamage(0d1+3): count=0 → zero dice rolls, bonus=3.
    let self_dmg = DiceExpr::new(0, 1, 3);
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext {
            crit_fail_outcome: CritFailOutcome::SelfDamage(self_dmg),
            ..Default::default()
        },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // d20=1 → crit-fail; SelfDamage roll = 0 dice → only bonus, no roll_d call.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]);

    step(&mut state, action, &mut rng, &content).expect("should succeed");

    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 7, "caster hp = 10 - 3 = 7");
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 10, "target hp unchanged");
}

/// d20 = 1 + ApplyStatus("exhaustion") → caster gets exhaustion for 3 rounds.
#[test]
fn cast_crit_fail_apply_status() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext {
            crit_fail_outcome: CritFailOutcome::ApplyStatus(StatusId::from("exhaustion")),
            ..Default::default()
        },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // d20=1 → crit-fail ApplyStatus; no further dice.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]);

    step(&mut state, action, &mut rng, &content).expect("should succeed");

    let caster = state.unit(UnitId(1)).unwrap();
    assert_eq!(caster.statuses.len(), 1, "caster has one status");
    let s = &caster.statuses[0];
    assert_eq!(s.id, StatusId::from("exhaustion"));
    assert_eq!(s.rounds_remaining, 3, "fixed 3-round duration");
    assert_eq!(s.applier, combat_engine::state::EffectSource::Unit(UnitId(1)), "applier = caster");
    assert_eq!(state.unit(UnitId(2)).unwrap().statuses.len(), 0, "target unaffected");
}

/// d20 = 1 + any outcome → `Event::CritFailed` is emitted between ActionStarted and cost events.
#[test]
fn cast_crit_fail_emits_event() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext { crit_fail_outcome: CritFailOutcome::Miss, ..Default::default() },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]); // d20=1 → crit-fail

    let (events, _ctx) = step(&mut state, action, &mut rng, &content).expect("should succeed");

    let crit_failed = events.iter().any(|e| {
        matches!(e, Event::CritFailed { actor: UnitId(1), outcome: CritFailOutcome::Miss })
    });
    assert!(crit_failed, "CritFailed event must be present when d20=1; got: {:?}", events);

    // Ordering: CritFailed must come after ActionStarted, before ActionFinished.
    let started_pos = events.iter().position(|e| matches!(e, Event::ActionStarted { .. })).unwrap();
    let crit_pos = events.iter().position(|e| matches!(e, Event::CritFailed { .. })).unwrap();
    let finished_pos = events.iter().position(|e| matches!(e, Event::ActionFinished { .. })).unwrap();
    assert!(started_pos < crit_pos, "CritFailed must come after ActionStarted");
    assert!(crit_pos < finished_pos, "CritFailed must come before ActionFinished");
}

/// d20 = 11 (no crit-fail) → `Event::CritFailed` is NOT emitted.
#[test]
fn cast_no_crit_fail_no_event_when_d20_non_one() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext { crit_fail_outcome: CritFailOutcome::Miss, ..Default::default() },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let mut rng = DiceRng::with_seed(0);
    rng.script(&[11, 3]); // d20=11 (no crit-fail), damage=3

    let (events, _ctx) = step(&mut state, action, &mut rng, &content).expect("should succeed");

    let has_crit_failed = events.iter().any(|e| matches!(e, Event::CritFailed { .. }));
    assert!(!has_crit_failed, "CritFailed must NOT be present when d20=11; got: {:?}", events);
}

/// d20 = 11 (no crit-fail) → normal damage cast proceeds; target takes 3 damage.
#[test]
fn cast_proceeds_normally_when_d20_not_one() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome, DiceRng};

    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // hp=10, armor=0

    let mut state = state_with(vec![actor, target]);
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::Damage { dice: DiceExpr::new(1, 4, 0) },
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("strike", ability).with_caster(
        UnitId(1),
        CasterContext { crit_fail_outcome: CritFailOutcome::Miss, ..Default::default() },
    );
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // d20=11 → no crit-fail; damage roll 1d4=3 → raw=3+str_mod(0)=3.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[11, 3]);

    step(&mut state, action, &mut rng, &content).expect("should succeed");

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 7, "target hp = 10 - 3 = 7");
}

/// AoE status (on: Target, friendly_fire: false) applied to each enemy in range.
/// Three enemies within radius 1 all receive "burning" status.
#[test]
fn cast_applies_status_to_each_aoe_target() {
    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    let actor = make_unit(1, Team::Player, 0, 0);

    let mut e1 = make_unit(10, Team::Enemy, 0, 0);
    e1.pos = target_pos;
    let mut e2 = make_unit(11, Team::Enemy, 0, 0);
    e2.pos = neighbors[0];
    let mut e3 = make_unit(12, Team::Enemy, 0, 0);
    e3.pos = neighbors[1];

    let mut state = state_with(vec![actor, e1, e2, e3]);

    let ability = AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 8 },
        target_type: TargetType::Ground,
        aoe: AoEShape::Circle { radius: 1 },
        friendly_fire: false,
        effect: EffectDef::None,
        statuses: vec![StatusApplication {
            status: StatusId::from("burning"),
            duration_rounds: 2,
            on: StatusOn::Target,
        }],
        requires_los: false,
        passive: vec![],
    };
    let content = StubContent::with_ability("flame_wave", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("flame_wave"),
        target: UnitId(1),
        target_pos,
    };

    step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal AoE status cast should succeed");

    for id in [10u64, 11, 12] {
        let unit = state.unit(UnitId(id)).unwrap();
        assert_eq!(unit.statuses.len(), 1, "enemy {id} has burning status");
        assert_eq!(unit.statuses[0].id, StatusId::from("burning"));
        assert_eq!(unit.statuses[0].rounds_remaining, 2);
    }
}

// ── Step 3.5b tests: Summon ───────────────────────────────────────────────────

fn imp_template() -> UnitTemplate {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    UnitTemplate {
        max_hp: 8,
        armor: 1,
        base_speed: 4,
        max_ap: 1,
        mana_max: 0,
        energy_max: 0,
        rage_max: 0,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        initial_statuses: Vec::new(),
        initial_pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => None,
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => None,
            PoolKind::Mp     => None,
        },
    }
}

fn summon_ability(max_active: Option<u32>) -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 0 },
        target_type: TargetType::Myself,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::Summon { template_id: "imp".to_string(), max_active },
        statuses: vec![],
        requires_los: false,
        passive: vec![],
    }
}

/// Summon ability: step() emits UnitSpawned and inserts the new unit into state.
#[test]
fn cast_summon_ability_pushes_spawn_effect_and_creates_unit() {
    let summoner = make_unit(1, Team::Player, 2, 2);
    let summoner_id = summoner.id;
    let mut state = state_with(vec![summoner]);

    let content = StubContent::with_ability("summon_imp", summon_ability(Some(3)))
        .with_template("imp", imp_template());

    let action = Action::Cast {
        actor: summoner_id,
        ability: AbilityId::from("summon_imp"),
        target: summoner_id,
        target_pos: state.unit(summoner_id).unwrap().pos,
    };

    // d20=11 — no crit-fail.
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[11]);

    let (events, _ctx) = step(&mut state, action, &mut rng, &content).expect("summon should succeed");

    // Event stream contains UnitSpawned.
    let spawned = events.iter().find_map(|e| {
        if let Event::UnitSpawned { uid, summoner, template_id, team, .. } = e {
            Some((*uid, *summoner, template_id.clone(), *team))
        } else {
            None
        }
    });
    let (new_uid, s_id, tmpl_id, team) = spawned.expect("UnitSpawned event must be present");
    assert_eq!(s_id, summoner_id, "summoner field matches");
    assert_eq!(tmpl_id, "imp");
    assert_eq!(team, Team::Player, "new unit inherits summoner's team");

    // State now has 2 units.
    assert_eq!(state.alive_units().count(), 2, "one summoner + one summoned");

    // New unit stats match the template.
    let new_unit = state.unit(new_uid).expect("new unit present in state");
    assert_eq!(new_unit.max_hp(), 8);
    assert_eq!(new_unit.hp(), 8);
    assert_eq!(new_unit.armor, 1);
    assert_eq!(new_unit.summoner, Some(summoner_id));

    // Summoner paid AP.
    assert_eq!(state.unit(summoner_id).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP decremented by 1");
}

/// Crit-fail (d20=1, Miss outcome) on a Summon: cost paid, no spawn.
#[test]
fn cast_summon_crit_fail_miss_skips_spawn_but_pays_cost() {
    use storyforge::combat_engine::{CasterContext, CritFailOutcome};

    let summoner = make_unit(1, Team::Player, 2, 2);
    let summoner_id = summoner.id;
    let mut state = state_with(vec![summoner]);

    let content = StubContent::with_ability("summon_imp", summon_ability(Some(3)))
        .with_template("imp", imp_template())
        .with_caster(summoner_id, CasterContext {
            crit_fail_outcome: CritFailOutcome::Miss,
            ..Default::default()
        });
    content.apply_caster_contexts(&mut state);

    let action = Action::Cast {
        actor: summoner_id,
        ability: AbilityId::from("summon_imp"),
        target: summoner_id,
        target_pos: state.unit(summoner_id).unwrap().pos,
    };

    let mut rng = DiceRng::with_seed(0);
    rng.script(&[1]); // d20=1 → crit-fail

    let (events, _ctx) = step(&mut state, action, &mut rng, &content).expect("should succeed");

    // AP still paid.
    assert_eq!(state.unit(summoner_id).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP paid on crit-fail");
    // Unit count unchanged.
    assert_eq!(state.alive_units().count(), 1, "no unit spawned on crit-fail");
    // CritFailed event present, no UnitSpawned.
    assert!(events.iter().any(|e| matches!(e, Event::CritFailed { .. })), "CritFailed emitted");
    assert!(!events.iter().any(|e| matches!(e, Event::UnitSpawned { .. })), "no UnitSpawned on crit-fail");
}

/// Summon when max_active cap already reached → SpawnBlocked(MaxActiveReached); no new unit.
#[test]
fn cast_summon_at_max_cap_emits_spawn_blocked() {
    let summoner = make_unit(1, Team::Player, 2, 2);
    let summoner_id = summoner.id;

    // Pre-populate with one active summon at cap (max_active=1).
    let mut existing_summon = make_unit(2, Team::Player, 3, 2);
    existing_summon.summoner = Some(summoner_id);
    let mut state = state_with(vec![summoner, existing_summon]);

    let content = StubContent::with_ability("summon_imp", summon_ability(Some(1)))
        .with_template("imp", imp_template());

    let action = Action::Cast {
        actor: summoner_id,
        ability: AbilityId::from("summon_imp"),
        target: summoner_id,
        target_pos: state.unit(summoner_id).unwrap().pos,
    };

    let mut rng = DiceRng::with_seed(0);
    rng.script(&[11]); // no crit-fail

    let (events, _ctx) = step(&mut state, action, &mut rng, &content).expect("should succeed");

    // SpawnBlocked event with MaxActiveReached.
    let blocked = events.iter().find_map(|e| {
        if let Event::SpawnBlocked { reason, .. } = e { Some(reason.clone()) } else { None }
    });
    assert_eq!(blocked, Some(SpawnBlockedReason::MaxActiveReached), "SpawnBlocked(MaxActiveReached)");
    // Unit count unchanged — still 2 (summoner + existing summon).
    assert_eq!(state.alive_units().count(), 2, "no new unit inserted when cap reached");
}


// ── Phase B-α: lock-in tests (pre-S6 contract) ────────────────────────────────

/// Phase B-γ: S6 is now live — engine emits `TurnEnded{cause: ResourcesExhausted}`
/// and advances the turn when AP+MP are exhausted after a Cast.
///
/// Inverted from B-α canary `cast_exhausting_ap_mp_does_not_auto_advance_in_engine`.
#[test]
fn cast_exhausting_ap_mp_self_advances_in_engine_s6() {
    let mut actor = make_unit(1, Team::Player, 0, 0);
    // Override defaults: 1 AP, 0 MP — cast costs 1 AP, leaving AP=0, MP=0.
    actor.pools[storyforge::combat_engine::PoolKind::Ap] = Some((1, 1));
    actor.pools[storyforge::combat_engine::PoolKind::Mp] = Some((0, 6));

    let target = make_unit(2, Team::Enemy, 1, 0);

    let mut state = state_with(vec![actor, target]);
    // Set actor as the current combatant so we can assert the cursor advances.
    state.set_turn_queue(vec![UnitId(1), UnitId(2)], 0);

    // Ability: costs 1 AP, no damage, targets a single enemy.
    let ability = AbilityDef {
        cost_ap: 1,
        costs: vec![],
        effect: EffectDef::None,
        ..single_enemy_ability()
    };
    let content = StubContent::with_ability("exhausting_strike", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("exhausting_strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let (events, _ctx) = step(&mut state, action, &mut ExpectedValue, &content)
        .expect("legal cast should succeed");

    // S6: engine MUST emit exactly one TurnEnded with cause ResourcesExhausted.
    use combat_engine::event::TurnEndCause;
    let turn_ended_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Event::TurnEnded { .. }))
        .collect();
    assert_eq!(
        turn_ended_events.len(),
        1,
        "engine must emit exactly one TurnEnded on AP/MP exhaustion (S6); events: {:?}",
        events,
    );
    assert!(
        matches!(
            turn_ended_events[0],
            Event::TurnEnded { cause: TurnEndCause::ResourcesExhausted, .. }
        ),
        "TurnEnded cause must be ResourcesExhausted; got: {:?}",
        turn_ended_events[0],
    );

    // Turn cursor must have advanced to actor 2 (index=1).
    assert_eq!(
        state.turn_queue.current(),
        Some(UnitId(2)),
        "turn cursor must advance after AP/MP exhaustion (S6); queue: {:?}",
        state.turn_queue,
    );

    // Sanity: TurnStarted for actor 2 is also in the stream.
    assert!(
        events.iter().any(|e| matches!(e, Event::TurnStarted { actor: UnitId(2) })),
        "TurnStarted for actor 2 must be emitted; events: {:?}",
        events,
    );

    // Sanity: AP was actually consumed.
    assert!(
        state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0) <= 0,
        "AP must be exhausted after the cast",
    );
}
