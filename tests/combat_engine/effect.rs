//! Step 4 unit tests: `apply_effect` for all 7 Effect variants.
//!
//! Decision 6.3 (per-target ordering) is pinned by `damage_derives_rage_then_death`.
//! Decision 6.5 (strict failure) is tested in `engine_step.rs`.

use storyforge::combat_engine::{
    content::{ContentView, StatusBonuses},
    dice::DiceExpr,
    effect::{apply_effect, Effect},
    state::{ActiveStatus, CombatState, RoundPhase, Team, Unit, UnitId},
};
use storyforge::combat_engine::StatusId;
use storyforge::game::hex::hex_from_offset;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal `ContentView` stub for effect tests.
struct StubContent {
    speed_bonus: i32,
    armor_bonus: i32,
}

impl StubContent {
    fn neutral() -> Self {
        Self { speed_bonus: 0, armor_bonus: 0 }
    }
    fn with_speed(speed_bonus: i32) -> Self {
        Self { speed_bonus, armor_bonus: 0 }
    }
    fn with_armor(armor_bonus: i32) -> Self {
        Self { speed_bonus: 0, armor_bonus }
    }
}

impl ContentView for StubContent {
    fn aoo_dice(&self, _: UnitId) -> Option<DiceExpr> {
        None
    }
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses {
        StatusBonuses {
            speed_bonus: self.speed_bonus,
            armor_bonus: self.armor_bonus,
        }
    }
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId) -> Option<storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<storyforge::combat_engine::StatusDef> { None }
    fn caster_context(&self, _: UnitId) -> storyforge::combat_engine::CasterContext { storyforge::combat_engine::CasterContext::default() }
}

fn make_unit(id: u64, hp: i32, max_hp: i32) -> Unit {
    Unit {
        id: UnitId(id),
        team: Team::Player,
        pos: hex_from_offset(0, 0),
        hp,
        max_hp,
        armor: 0,
        armor_bonus: 0,
        base_speed: 4,
        speed: 4,
        action_points: 2,
        movement_points: 4,
        reactions_left: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
    }
}

fn unit_with_rage(id: u64, rage_current: i32, rage_max: i32) -> Unit {
    let mut u = make_unit(id, 20, 20);
    u.rage = Some((rage_current, rage_max));
    u
}

fn state_with(units: Vec<Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::ActorTurn, 0)
}

// ── MovePosition ──────────────────────────────────────────────────────────────

#[test]
fn move_position_updates_pos() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);
    let dest = hex_from_offset(3, 2);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::MovePosition { actor: UnitId(1), to: dest },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().pos, dest);
    assert!(derived.is_empty());
}

// ── DecrementMP ───────────────────────────────────────────────────────────────

#[test]
fn decrement_mp_clamps_at_zero() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::DecrementMP { actor: UnitId(1), by: 100 },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().movement_points, 0);
    assert!(derived.is_empty());
}

// ── Damage ────────────────────────────────────────────────────────────────────

/// Decision 6.3: Damage derives GainRage{source}, GainRage{target} for non-lethal.
#[test]
fn damage_nonlethal_derives_rage_source_then_target() {
    let attacker = make_unit(1, 20, 20);
    let defender = make_unit(2, 20, 20);
    let mut state = state_with(vec![attacker, defender]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Damage { target: UnitId(2), raw: 5.0, source: UnitId(1), pierces: false },
        &StubContent::neutral(),
    );

    // No armor → final_damage = max(1, 5) = 5; hp 20 - 5 = 15 (alive).
    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 15);
    assert!(ctx.final_damage.is_some());

    // Derived: GainRage{source=1}, GainRage{target=2} — no Death.
    assert_eq!(derived.len(), 2);
    assert!(matches!(derived[0], Effect::GainRage { target } if target == UnitId(1)));
    assert!(matches!(derived[1], Effect::GainRage { target } if target == UnitId(2)));
}

/// Decision 6.3: Damage derives GainRage{source}, GainRage{target}, Death{target}
/// for lethal damage — in that exact order.
#[test]
fn damage_lethal_derives_rage_source_rage_target_death_target_in_order() {
    let attacker = make_unit(1, 20, 20);
    let defender = make_unit(2, 3, 20); // 3 hp — dies from 10 raw
    let mut state = state_with(vec![attacker, defender]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: UnitId(2), raw: 10.0, source: UnitId(1), pierces: false },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 0);
    assert_eq!(derived.len(), 3);
    assert!(matches!(derived[0], Effect::GainRage { target } if target == UnitId(1)));
    assert!(matches!(derived[1], Effect::GainRage { target } if target == UnitId(2)));
    assert!(matches!(derived[2], Effect::Death { unit } if unit == UnitId(2)));
}

/// Armor reduces damage; min-1 floor still applies.
#[test]
fn damage_armor_reduces_final_damage() {
    let mut attacker = make_unit(1, 20, 20);
    attacker.armor = 8;
    let defender = make_unit(2, 20, 20);
    let mut state = state_with(vec![attacker, defender]);

    // Source has armor=8, target has armor=0. raw=5 on target → final = max(1,5)=5.
    apply_effect(
        &mut state,
        &Effect::Damage { target: UnitId(2), raw: 5.0, source: UnitId(1), pierces: false },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(2)).unwrap().hp, 15);

    // Now damage with raw=3, armor_bonus=5 on target.
    let mut heavy = make_unit(3, 20, 20);
    heavy.armor = 5;
    state = state_with(vec![heavy]);
    apply_effect(
        &mut state,
        &Effect::Damage { target: UnitId(3), raw: 3.0, source: UnitId(3), pierces: false },
        &StubContent::neutral(),
    );
    // final_damage_f32(3.0, 5.0, 0.0, false) = max(1, 3-5) = 1
    assert_eq!(state.unit(UnitId(3)).unwrap().hp, 19);
}

/// Armor-piercing ignores armor.
#[test]
fn damage_pierces_ignores_armor() {
    let mut u = make_unit(1, 20, 20);
    u.armor = 10;
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::Damage { target: UnitId(1), raw: 8.0, source: UnitId(1), pierces: true },
        &StubContent::neutral(),
    );
    // pierces=true → armor ignored: final = max(1, 8) = 8; hp = 20-8 = 12
    assert_eq!(state.unit(UnitId(1)).unwrap().hp, 12);
}

// ── GainRage ──────────────────────────────────────────────────────────────────

#[test]
fn gain_rage_increments_and_clamps_at_max() {
    let u = unit_with_rage(1, 4, 5);
    let mut state = state_with(vec![u]);

    apply_effect(&mut state, &Effect::GainRage { target: UnitId(1) }, &StubContent::neutral());
    assert_eq!(state.unit(UnitId(1)).unwrap().rage, Some((5, 5)));

    // One more gain — already at max, should stay at 5.
    apply_effect(&mut state, &Effect::GainRage { target: UnitId(1) }, &StubContent::neutral());
    assert_eq!(state.unit(UnitId(1)).unwrap().rage, Some((5, 5)));
}

#[test]
fn gain_rage_noop_when_unit_has_no_rage() {
    let u = make_unit(1, 10, 10); // rage = None
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::GainRage { target: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().rage, None);
    assert!(derived.is_empty());
}

// ── DecrementReactions ────────────────────────────────────────────────────────

#[test]
fn decrement_reactions_clamps_at_zero() {
    let u = make_unit(1, 10, 10); // reactions_left = 1
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::DecrementReactions { actor: UnitId(1) },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().reactions_left, 0);

    // Second decrement — clamp at 0.
    apply_effect(
        &mut state,
        &Effect::DecrementReactions { actor: UnitId(1) },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().reactions_left, 0);
}

// ── Death ─────────────────────────────────────────────────────────────────────

#[test]
fn death_sets_hp_to_zero_and_unit_is_dead() {
    let mut u = make_unit(1, 0, 20); // hp already 0 (set by preceding Damage)
    u.hp = 0;
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Death { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    assert!(!state.unit(UnitId(1)).unwrap().is_alive());
    assert!(derived.is_empty());
}

// ── RefreshAggregates ─────────────────────────────────────────────────────────

/// RefreshAggregates with haste-like status (speed_bonus = +2) bumps speed.
#[test]
fn refresh_aggregates_recomputes_speed_from_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.base_speed = 3;
    u.speed = 3;
    u.statuses = vec![ActiveStatus {
        id: "haste".into(),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: UnitId(1),
    }];
    let mut state = state_with(vec![u]);

    // ContentView reports speed_bonus=+2 for any status.
    apply_effect(
        &mut state,
        &Effect::RefreshAggregates { unit: UnitId(1) },
        &StubContent::with_speed(2),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().speed, 5); // 3 + 2
}

/// RefreshAggregates with armor-buff status bumps armor_bonus.
#[test]
fn refresh_aggregates_recomputes_armor_bonus_from_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.armor = 2;
    u.armor_bonus = 0;
    u.statuses = vec![ActiveStatus {
        applier: UnitId(1),
        id: "iron_skin".into(),
        rounds_remaining: 1,
        dot_per_tick: 0,
    }];
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::RefreshAggregates { unit: UnitId(1) },
        &StubContent::with_armor(3),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().armor_bonus, 3);
    assert_eq!(state.unit(UnitId(1)).unwrap().speed, 4); // unchanged
}

/// No statuses → RefreshAggregates resets bonuses to zero.
#[test]
fn refresh_aggregates_clears_bonuses_when_no_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.armor_bonus = 5; // stale from before status expired
    u.speed = 10;      // stale
    u.base_speed = 4;
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::RefreshAggregates { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().armor_bonus, 0);
    assert_eq!(state.unit(UnitId(1)).unwrap().speed, 4); // base_speed
}

// ── Heal ─────────────────────────────────────────────────────────────────────

/// Heal with no DoT statuses: HP restored, capped at max_hp.
#[test]
fn heal_no_dot_restores_hp() {
    let mut u = make_unit(1, 3, 10);
    let mut state = state_with(vec![u.clone()]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal { target: UnitId(1), amount: 5 },
        &StubContent::neutral(),
    );

    assert!(derived.is_empty(), "no status removed → no derived effects");
    assert_eq!(ctx.heal_amount, Some(5), "5 HP restored");
    assert_eq!(state.unit(UnitId(1)).unwrap().hp, 8, "3 + 5 = 8");

    // Sanity: heals above cap clamp.
    u.hp = 8;
    state = state_with(vec![u]);
    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Heal { target: UnitId(1), amount: 10 },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().hp, 10, "clamped at max");
    assert_eq!(ctx.heal_amount, Some(2), "only 2 HP actually restored (10 - 8)");
}

/// Heal pool exceeds DoT: status removed, remaining heal restores HP.
/// Decision 6.x parity with `apply_effects_system`: DoT-neutralize first,
/// then HP heal. Status removal derives `RefreshAggregates` so any
/// armor/speed bonuses from the cleansed status are cleared.
#[test]
fn heal_full_neutralizes_dot_then_restores_hp() {
    let mut u = make_unit(1, 3, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 2,
        applier: UnitId(2),
    });
    let mut state = state_with(vec![u]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal { target: UnitId(1), amount: 5 },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert!(unit.statuses.is_empty(), "poison neutralized + removed");
    assert_eq!(unit.hp, 6, "3 + (5 - 2 DoT) = 6");
    assert_eq!(ctx.heal_amount, Some(3), "3 HP actually restored");
    assert_eq!(derived.len(), 1, "RefreshAggregates derived from status removal");
    assert!(matches!(derived[0], Effect::RefreshAggregates { unit: UnitId(1) }));
}

/// Heal pool smaller than DoT: status partially weakened, no HP heal.
#[test]
fn heal_partial_dot_consumes_all_heal_no_hp_restored() {
    let mut u = make_unit(1, 3, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 8,
        applier: UnitId(2),
    });
    let mut state = state_with(vec![u]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal { target: UnitId(1), amount: 3 },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "poison still active");
    assert_eq!(unit.statuses[0].dot_per_tick, 5, "8 - 3 = 5 dot remaining");
    assert_eq!(unit.hp, 3, "no HP restored — pool consumed by DoT");
    assert_eq!(ctx.heal_amount, Some(0));
    assert!(derived.is_empty(), "no status removed → no RefreshAggregates");
}

/// Multiple DoT statuses: heal cleanses them in order until pool exhausts.
#[test]
fn heal_neutralizes_multiple_dots_in_order() {
    let mut u = make_unit(1, 1, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 2,
        applier: UnitId(2),
    });
    u.statuses.push(ActiveStatus {
        id: StatusId::from("burning"),
        rounds_remaining: 2,
        dot_per_tick: 3,
        applier: UnitId(2),
    });
    let mut state = state_with(vec![u]);

    // Heal pool = 4: cleanses poison (2), leaves burning weakened (3 - 2 = 1).
    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Heal { target: UnitId(1), amount: 4 },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "poison removed, burning remains");
    assert_eq!(unit.statuses[0].id, StatusId::from("burning"));
    assert_eq!(unit.statuses[0].dot_per_tick, 1, "burning weakened to 1");
    assert_eq!(unit.hp, 1, "no HP restored — pool consumed by DoTs");
    assert_eq!(ctx.heal_amount, Some(0));
}

// ── PayCost ──────────────────────────────────────────────────────────────────

/// PayCost decrements the matching resource pool, clamped at 0.
#[test]
fn pay_cost_decrements_mana() {
    let mut u = make_unit(1, 10, 10);
    u.mana = Some((8, 10));
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::PayCost {
            actor: UnitId(1),
            kind: storyforge::combat_engine::ResourceKind::Mana,
            amount: 3,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().mana, Some((5, 10)));
}

#[test]
fn pay_cost_clamps_pool_at_zero() {
    let mut u = make_unit(1, 10, 10);
    u.rage = Some((2, 5));
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::PayCost {
            actor: UnitId(1),
            kind: storyforge::combat_engine::ResourceKind::Rage,
            amount: 99,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().rage, Some((0, 5)));
}

#[test]
fn pay_cost_hp_decrements_directly() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);
    apply_effect(
        &mut state,
        &Effect::PayCost {
            actor: UnitId(1),
            kind: storyforge::combat_engine::ResourceKind::Hp,
            amount: 4,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().hp, 6);
}

#[test]
fn pay_cost_skips_when_pool_absent() {
    let u = make_unit(1, 10, 10); // mana: None
    let mut state = state_with(vec![u]);
    // Should not panic; pool stays None.
    apply_effect(
        &mut state,
        &Effect::PayCost {
            actor: UnitId(1),
            kind: storyforge::combat_engine::ResourceKind::Mana,
            amount: 5,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().mana, None);
}

// ── ApplyStatus ──────────────────────────────────────────────────────────────

/// ApplyStatus adds a new entry + derives RefreshAggregates.
#[test]
fn apply_status_pushes_new_entry() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::ApplyStatus {
            target: UnitId(1),
            status: StatusId::from("poison"),
            rounds: 3,
            dot_per_tick: 2,
            applier: UnitId(2),
        },
        &StubContent::neutral(),
    );
    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1);
    assert_eq!(unit.statuses[0].id, StatusId::from("poison"));
    assert_eq!(unit.statuses[0].rounds_remaining, 3);
    assert_eq!(unit.statuses[0].dot_per_tick, 2);
    assert_eq!(unit.statuses[0].applier, UnitId(2));
    assert_eq!(derived.len(), 1);
    assert!(matches!(derived[0], Effect::RefreshAggregates { unit: UnitId(1) }));
}

/// Re-applying same status id replaces the existing entry (matches
/// `apply_effects_system` reapply semantics — see
/// `tests/combat/statuses.rs::reapplying_status_replaces_previous`).
#[test]
fn apply_status_replaces_existing_with_same_id() {
    let mut u = make_unit(1, 10, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("burning"),
        rounds_remaining: 1, // about to expire
        dot_per_tick: 1,
        applier: UnitId(3),
    });
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::ApplyStatus {
            target: UnitId(1),
            status: StatusId::from("burning"),
            rounds: 4, // refreshed duration
            dot_per_tick: 3,
            applier: UnitId(2),
        },
        &StubContent::neutral(),
    );
    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "still one burning entry — replaced not appended");
    assert_eq!(unit.statuses[0].rounds_remaining, 4);
    assert_eq!(unit.statuses[0].dot_per_tick, 3);
    assert_eq!(unit.statuses[0].applier, UnitId(2));
}

// ── RemoveStatus ─────────────────────────────────────────────────────────────

#[test]
fn remove_status_filters_by_id_and_derives_refresh() {
    let mut u = make_unit(1, 10, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("haste"),
        rounds_remaining: 3,
        dot_per_tick: 0,
        applier: UnitId(2),
    });
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 5,
        dot_per_tick: 2,
        applier: UnitId(2),
    });
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::RemoveStatus {
            target: UnitId(1),
            status: StatusId::from("haste"),
        },
        &StubContent::neutral(),
    );
    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1);
    assert_eq!(unit.statuses[0].id, StatusId::from("poison"));
    assert_eq!(derived.len(), 1);
    assert!(matches!(derived[0], Effect::RefreshAggregates { unit: UnitId(1) }));
}

/// RemoveStatus on a unit that doesn't have the status: no-op, no derived.
#[test]
fn remove_status_noop_when_absent() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::RemoveStatus {
            target: UnitId(1),
            status: StatusId::from("nonexistent"),
        },
        &StubContent::neutral(),
    );
    assert!(derived.is_empty(), "no removal → no RefreshAggregates");
    assert!(state.unit(UnitId(1)).unwrap().statuses.is_empty());
}
