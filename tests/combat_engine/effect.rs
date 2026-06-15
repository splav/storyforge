//! Step 4 unit tests: `apply_effect` for all 7 Effect variants.
//!
//! Decision 6.3 (per-target ordering) is pinned by `damage_derives_rage_then_death`.
//! Decision 6.5 (strict failure) is tested in `engine_step.rs`.

use storyforge::combat_engine::StatusId;
use storyforge::combat_engine::{
    content::{ContentView, StatusBonuses},
    effect::{apply_effect, ApplyCtx, Effect},
    event::{effect_to_event, Event},
    state::{ActiveStatus, CombatState, EffectSource, RoundPhase, Unit, UnitId},
};
use storyforge::game::hex::hex_from_offset;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal `ContentView` stub for effect tests.
struct StubContent {
    speed_bonus: i32,
    armor_bonus: i32,
    templates: std::collections::HashMap<String, storyforge::combat_engine::UnitTemplate>,
    cached_status_def: storyforge::combat_engine::StatusDef,
}

impl StubContent {
    fn make_status_def(
        speed_bonus: i32,
        armor_bonus: i32,
        hp_percent_dot: i32,
    ) -> storyforge::combat_engine::StatusDef {
        storyforge::combat_engine::StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: false,
            bonuses: storyforge::combat_engine::StatusBonuses {
                runtime: storyforge::combat_engine::RuntimeStatsDelta(
                    storyforge::combat_engine::RuntimeStats {
                        armor: armor_bonus,
                        magic_resist: 0,
                        base_speed: speed_bonus,
                    },
                ),
                damage_taken_bonus: 0,
            },
            hp_percent_dot,
            heal_per_tick: 0,
        }
    }
    fn neutral() -> Self {
        let d = Self::make_status_def(0, 0, 0);
        Self {
            speed_bonus: 0,
            armor_bonus: 0,
            templates: Default::default(),
            cached_status_def: d,
        }
    }
    fn with_speed(speed_bonus: i32) -> Self {
        let d = Self::make_status_def(speed_bonus, 0, 0);
        Self {
            speed_bonus,
            armor_bonus: 0,
            templates: Default::default(),
            cached_status_def: d,
        }
    }
    fn with_armor(armor_bonus: i32) -> Self {
        let d = Self::make_status_def(0, armor_bonus, 0);
        Self {
            speed_bonus: 0,
            armor_bonus,
            templates: Default::default(),
            cached_status_def: d,
        }
    }
    fn with_hp_percent_dot(hp_percent_dot: i32) -> Self {
        let d = Self::make_status_def(0, 0, hp_percent_dot);
        Self {
            speed_bonus: 0,
            armor_bonus: 0,
            templates: Default::default(),
            cached_status_def: d,
        }
    }
    fn with_template(mut self, id: &str, tpl: storyforge::combat_engine::UnitTemplate) -> Self {
        self.templates.insert(id.to_string(), tpl);
        self
    }
}

impl ContentView for StubContent {
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses {
        StatusBonuses {
            runtime: storyforge::combat_engine::RuntimeStatsDelta(
                storyforge::combat_engine::RuntimeStats {
                    armor: self.armor_bonus,
                    magic_resist: 0,
                    base_speed: self.speed_bonus,
                },
            ),
            damage_taken_bonus: 0,
        }
    }
    fn ability_def(
        &self,
        _: &storyforge::combat_engine::AbilityId,
    ) -> Option<&storyforge::combat_engine::AbilityDef> {
        None
    }
    fn status_def(&self, _: &StatusId) -> Option<&storyforge::combat_engine::StatusDef> {
        Some(&self.cached_status_def)
    }
    fn unit_template(&self, id: &str) -> Option<storyforge::combat_engine::UnitTemplate> {
        self.templates.get(id).cloned()
    }
}

fn make_unit(id: u64, hp: i32, max_hp: i32) -> Unit {
    crate::common::engine_unit::EngineUnitBuilder::new(id)
        .speed(4)
        .hp(hp, max_hp)
        .mp(4, 4)
        .build()
}

fn unit_with_rage(id: u64, rage_current: i32, rage_max: i32) -> Unit {
    let mut u = make_unit(id, 20, 20);
    u.pools[storyforge::combat_engine::PoolKind::Rage] = Some((rage_current, rage_max));
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
        &Effect::MovePosition {
            actor: UnitId(1),
            to: dest,
        },
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
        &Effect::DecrementMP {
            actor: UnitId(1),
            by: 100,
        },
        &StubContent::neutral(),
    );

    let mp = state.unit(UnitId(1)).unwrap().pools[storyforge::combat_engine::PoolKind::Mp]
        .map(|(c, _)| c)
        .unwrap_or(0);
    assert_eq!(mp, 0);
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
        &Effect::Damage {
            target: UnitId(2),
            raw: 5.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: false,
            magic: false,
        },
        &StubContent::neutral(),
    );

    // No armor → final_damage = max(1, 5) = 5; hp 20 - 5 = 15 (alive).
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 15);
    assert!(ctx.damage.is_some());

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
        &Effect::Damage {
            target: UnitId(2),
            raw: 10.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: false,
            magic: false,
        },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 0);
    assert_eq!(derived.len(), 3);
    assert!(matches!(derived[0], Effect::GainRage { target } if target == UnitId(1)));
    assert!(matches!(derived[1], Effect::GainRage { target } if target == UnitId(2)));
    assert!(matches!(derived[2], Effect::Death { unit } if unit == UnitId(2)));
}

/// Armor reduces damage; min-1 floor still applies.
#[test]
fn damage_armor_reduces_final_damage() {
    let mut attacker = make_unit(1, 20, 20);
    attacker.runtime.armor = 8;
    let defender = make_unit(2, 20, 20);
    let mut state = state_with(vec![attacker, defender]);

    // Source has armor=8, target has armor=0. raw=5 on target → final = max(1,5)=5.
    apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(2),
            raw: 5.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: false,
            magic: false,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(2)).unwrap().hp(), 15);

    // Now damage with raw=3, armor_bonus=5 on target.
    let mut heavy = make_unit(3, 20, 20);
    heavy.runtime.armor = 5;
    state = state_with(vec![heavy]);
    apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(3),
            raw: 3.0,
            source: EffectSource::Unit(UnitId(3)),
            pierces: false,
            magic: false,
        },
        &StubContent::neutral(),
    );
    // final_damage_f32(3.0, 5.0, 0.0, false) = max(1, 3-5) = 1
    assert_eq!(state.unit(UnitId(3)).unwrap().hp(), 19);
}

/// Armor-piercing ignores armor.
#[test]
fn damage_pierces_ignores_armor() {
    let mut u = make_unit(1, 20, 20);
    u.runtime.armor = 10;
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(1),
            raw: 8.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: true,
            magic: false,
        },
        &StubContent::neutral(),
    );
    // pierces=true → armor ignored: final = max(1, 8) = 8; hp = 20-8 = 12
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 12);
}

/// Magic damage (magic=true) is reduced by magic_resist, not armor.
/// Parameterized: (raw, magic_resist, armor, expected_hp).
#[test]
fn damage_magic_reduced_by_magic_resist() {
    // (raw, magic_resist, armor, hp_start=20, expected_hp_after)
    let cases: &[(f32, i32, i32, i32)] = &[
        (10.0, 3, 0, 13), // dealt=max(1,10-3)=7; hp=20-7=13
        (10.0, 3, 8, 13), // armor ignored (magic); same dealt=7; hp=13
        (2.0, 5, 0, 19),  // dealt=max(1,2-5)=1 (floor); hp=20-1=19
        (5.0, 0, 4, 15),  // magic_resist=0 → no mitigation; dealt=5; hp=15
    ];

    for &(raw, mr, armor, expected) in cases {
        let u = crate::common::engine_unit::EngineUnitBuilder::new(1)
            .hp(20, 20)
            .armor(armor)
            .magic_resist(mr)
            .build();
        let mut state = state_with(vec![u]);
        apply_effect(
            &mut state,
            &Effect::Damage {
                target: UnitId(1),
                raw,
                source: EffectSource::Unit(UnitId(1)),
                pierces: false,
                magic: true,
            },
            &StubContent::neutral(),
        );
        assert_eq!(
            state.unit(UnitId(1)).unwrap().hp(),
            expected,
            "raw={raw} mr={mr} armor={armor} expected={expected}"
        );
    }
}

/// Physical damage (magic=false) is NOT reduced by magic_resist.
#[test]
fn damage_physical_ignores_magic_resist() {
    let mut u = make_unit(1, 20, 20);
    u.runtime.magic_resist = 10; // high magic_resist must not affect physical damage
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(1),
            raw: 8.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: false,
            magic: false,
        },
        &StubContent::neutral(),
    );
    // armor=0 (make_unit default), magic_resist ignored: final = max(1, 8) = 8; hp = 12
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 12);
}

/// Armor does NOT reduce magic damage.
#[test]
fn damage_armor_does_not_reduce_magic() {
    let mut u = make_unit(1, 20, 20);
    u.runtime.armor = 10; // high armor must not mitigate magic damage
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(1),
            raw: 8.0,
            source: EffectSource::Unit(UnitId(1)),
            pierces: false,
            magic: true,
        },
        &StubContent::neutral(),
    );
    // magic_resist=0 (default), armor ignored: final = max(1, 8) = 8; hp = 12
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 12);
}

// ── GainRage ──────────────────────────────────────────────────────────────────

#[test]
fn gain_rage_increments_and_clamps_at_max() {
    use storyforge::combat_engine::PoolKind;
    let u = unit_with_rage(1, 4, 5);
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::GainRage { target: UnitId(1) },
        &StubContent::neutral(),
    );
    assert_eq!(
        state.unit(UnitId(1)).unwrap().pools[PoolKind::Rage],
        Some((5, 5))
    );

    // One more gain — already at max, should stay at 5.
    apply_effect(
        &mut state,
        &Effect::GainRage { target: UnitId(1) },
        &StubContent::neutral(),
    );
    assert_eq!(
        state.unit(UnitId(1)).unwrap().pools[PoolKind::Rage],
        Some((5, 5))
    );
}

#[test]
fn gain_rage_noop_when_unit_has_no_rage() {
    use storyforge::combat_engine::PoolKind;
    let u = make_unit(1, 10, 10); // rage = None
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::GainRage { target: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().pools[PoolKind::Rage], None);
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
    // Start from positive HP so the Death effect actually drives hp to zero.
    let u = make_unit(1, 10, 20);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Death { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 0);
    assert!(!state.unit(UnitId(1)).unwrap().is_alive());
    assert!(derived.is_empty());
}

// ── RefreshAggregates ─────────────────────────────────────────────────────────

/// RefreshAggregates with haste-like status (speed_bonus = +2) bumps speed.
#[test]
fn refresh_aggregates_recomputes_speed_from_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.runtime.base_speed = 3;
    u.runtime_bonus.0.base_speed = 99; // stale; RefreshAggregates must overwrite
    u.statuses = vec![ActiveStatus {
        id: "haste".into(),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: EffectSource::Unit(UnitId(1)),
    }];
    let mut state = state_with(vec![u]);

    // ContentView reports speed_bonus=+2 for any status.
    apply_effect(
        &mut state,
        &Effect::RefreshAggregates { unit: UnitId(1) },
        &StubContent::with_speed(2),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().effective_speed(), 5); // 3 + 2
}

/// RefreshAggregates with armor-buff status bumps armor_bonus.
#[test]
fn refresh_aggregates_recomputes_armor_bonus_from_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.runtime.armor = 2;
    u.runtime_bonus.0.armor = 99; // stale; RefreshAggregates must overwrite
    u.statuses = vec![ActiveStatus {
        applier: EffectSource::Unit(UnitId(1)),
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

    assert_eq!(state.unit(UnitId(1)).unwrap().runtime_bonus.0.armor, 3);
    assert_eq!(state.unit(UnitId(1)).unwrap().effective_speed(), 4); // unchanged
}

/// No statuses → RefreshAggregates resets bonuses to zero.
#[test]
fn refresh_aggregates_clears_bonuses_when_no_statuses() {
    let mut u = make_unit(1, 10, 10);
    u.runtime_bonus.0.armor = 5; // stale from before status expired
    u.runtime_bonus.0.base_speed = 6; // stale
    u.runtime.base_speed = 4;
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::RefreshAggregates { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(state.unit(UnitId(1)).unwrap().runtime_bonus.0.armor, 0);
    assert_eq!(state.unit(UnitId(1)).unwrap().effective_speed(), 4); // base_speed
}

// ── Heal ─────────────────────────────────────────────────────────────────────

/// Heal with no DoT statuses: HP restored, capped at max_hp.
#[test]
fn heal_no_dot_restores_hp() {
    let mut u = make_unit(1, 3, 10);
    let mut state = state_with(vec![u.clone()]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal {
            target: UnitId(1),
            amount: 5,
        },
        &StubContent::neutral(),
    );

    assert!(derived.is_empty(), "no status removed → no derived effects");
    assert_eq!(ctx.heal_amount, Some(5), "5 HP restored");
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 8, "3 + 5 = 8");

    // Sanity: heals above cap clamp.
    u.pools[storyforge::combat_engine::PoolKind::Hp] = Some((8, 10));
    state = state_with(vec![u]);
    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Heal {
            target: UnitId(1),
            amount: 10,
        },
        &StubContent::neutral(),
    );
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 10, "clamped at max");
    assert_eq!(
        ctx.heal_amount,
        Some(2),
        "only 2 HP actually restored (10 - 8)"
    );
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
        applier: EffectSource::Unit(UnitId(2)),
    });
    let mut state = state_with(vec![u]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal {
            target: UnitId(1),
            amount: 5,
        },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert!(unit.statuses.is_empty(), "poison neutralized + removed");
    assert_eq!(unit.hp(), 6, "3 + (5 - 2 DoT) = 6");
    assert_eq!(ctx.heal_amount, Some(3), "3 HP actually restored");
    assert_eq!(
        derived.len(),
        1,
        "RefreshAggregates derived from status removal"
    );
    assert!(matches!(
        derived[0],
        Effect::RefreshAggregates { unit: UnitId(1) }
    ));
}

/// Heal pool smaller than DoT: status partially weakened, no HP heal.
#[test]
fn heal_partial_dot_consumes_all_heal_no_hp_restored() {
    let mut u = make_unit(1, 3, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 8,
        applier: EffectSource::Unit(UnitId(2)),
    });
    let mut state = state_with(vec![u]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Heal {
            target: UnitId(1),
            amount: 3,
        },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "poison still active");
    assert_eq!(unit.statuses[0].dot_per_tick, 5, "8 - 3 = 5 dot remaining");
    assert_eq!(unit.hp(), 3, "no HP restored — pool consumed by DoT");
    assert_eq!(ctx.heal_amount, Some(0));
    assert!(
        derived.is_empty(),
        "no status removed → no RefreshAggregates"
    );
}

/// Multiple DoT statuses: heal cleanses them in order until pool exhausts.
#[test]
fn heal_neutralizes_multiple_dots_in_order() {
    let mut u = make_unit(1, 1, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 2,
        applier: EffectSource::Unit(UnitId(2)),
    });
    u.statuses.push(ActiveStatus {
        id: StatusId::from("burning"),
        rounds_remaining: 2,
        dot_per_tick: 3,
        applier: EffectSource::Unit(UnitId(2)),
    });
    let mut state = state_with(vec![u]);

    // Heal pool = 4: cleanses poison (2), leaves burning weakened (3 - 2 = 1).
    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Heal {
            target: UnitId(1),
            amount: 4,
        },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "poison removed, burning remains");
    assert_eq!(unit.statuses[0].id, StatusId::from("burning"));
    assert_eq!(unit.statuses[0].dot_per_tick, 1, "burning weakened to 1");
    assert_eq!(unit.hp(), 1, "no HP restored — pool consumed by DoTs");
    assert_eq!(ctx.heal_amount, Some(0));
}

// ── PayCost ──────────────────────────────────────────────────────────────────

/// PayCost decrements the matching resource pool, clamped at 0.
#[test]
fn pay_cost_decrements_mana() {
    use storyforge::combat_engine::PoolKind;
    let mut u = make_unit(1, 10, 10);
    u.pools[PoolKind::Mana] = Some((8, 10));
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
    assert_eq!(
        state.unit(UnitId(1)).unwrap().pools[PoolKind::Mana],
        Some((5, 10))
    );
}

#[test]
fn pay_cost_clamps_pool_at_zero() {
    use storyforge::combat_engine::PoolKind;
    let mut u = make_unit(1, 10, 10);
    u.pools[PoolKind::Rage] = Some((2, 5));
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
    assert_eq!(
        state.unit(UnitId(1)).unwrap().pools[PoolKind::Rage],
        Some((0, 5))
    );
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
    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 6);
}

#[test]
fn pay_cost_skips_when_pool_absent() {
    use storyforge::combat_engine::PoolKind;
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
    assert_eq!(state.unit(UnitId(1)).unwrap().pools[PoolKind::Mana], None);
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
            applier: EffectSource::Unit(UnitId(2)),
        },
        &StubContent::neutral(),
    );
    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1);
    assert_eq!(unit.statuses[0].id, StatusId::from("poison"));
    assert_eq!(unit.statuses[0].rounds_remaining, 3);
    assert_eq!(unit.statuses[0].dot_per_tick, 2);
    assert_eq!(unit.statuses[0].applier, EffectSource::Unit(UnitId(2)));
    assert_eq!(derived.len(), 1);
    assert!(matches!(
        derived[0],
        Effect::RefreshAggregates { unit: UnitId(1) }
    ));
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
        applier: EffectSource::Unit(UnitId(3)),
    });
    let mut state = state_with(vec![u]);

    apply_effect(
        &mut state,
        &Effect::ApplyStatus {
            target: UnitId(1),
            status: StatusId::from("burning"),
            rounds: 4, // refreshed duration
            dot_per_tick: 3,
            applier: EffectSource::Unit(UnitId(2)),
        },
        &StubContent::neutral(),
    );
    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(
        unit.statuses.len(),
        1,
        "still one burning entry — replaced not appended"
    );
    assert_eq!(unit.statuses[0].rounds_remaining, 4);
    assert_eq!(unit.statuses[0].dot_per_tick, 3);
    assert_eq!(unit.statuses[0].applier, EffectSource::Unit(UnitId(2)));
}

// ── RemoveStatus ─────────────────────────────────────────────────────────────

#[test]
fn remove_status_filters_by_id_and_derives_refresh() {
    let mut u = make_unit(1, 10, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("haste"),
        rounds_remaining: 3,
        dot_per_tick: 0,
        applier: EffectSource::Unit(UnitId(2)),
    });
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 5,
        dot_per_tick: 2,
        applier: EffectSource::Unit(UnitId(2)),
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
    assert!(matches!(
        derived[0],
        Effect::RefreshAggregates { unit: UnitId(1) }
    ));
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

// ── TickDot ───────────────────────────────────────────────────────────────────

fn status_with_dot(id: &str, dot_per_tick: i32, applier: u64) -> ActiveStatus {
    ActiveStatus {
        id: StatusId::from(id),
        rounds_remaining: 3,
        dot_per_tick,
        applier: EffectSource::Unit(UnitId(applier)),
    }
}

/// Flat dot_per_tick produces piercing damage applied inline and populates ApplyCtx.dot_damage.
#[test]
fn tick_dot_with_dot_per_tick_damages_target_via_pierce() {
    let mut target = make_unit(1, 10, 10);
    target.runtime.armor = 5; // armor must be ignored (pierces = true)
    target.statuses.push(status_with_dot("poison", 3, 2));
    let applier = make_unit(2, 10, 10);
    let mut state = state_with(vec![target, applier]);

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::TickDot {
            target: UnitId(1),
            status: StatusId::from("poison"),
        },
        &StubContent::neutral(),
    );

    // HP reduced by 3 (armor ignored, pierce = true).
    assert_eq!(
        state.unit(UnitId(1)).unwrap().hp(),
        7,
        "HP should be reduced by 3"
    );

    // dot_damage ctx carries the fused breakdown.
    let dot = ctx
        .dot_damage
        .as_ref()
        .expect("dot_damage must be populated");
    assert_eq!(dot.source, EffectSource::Unit(UnitId(2)));
    assert_eq!(dot.source_status, StatusId::from("poison"));
    assert!((dot.raw - 3.0).abs() < f32::EPSILON);
    assert_eq!(dot.mitigation, 0);
    assert!(dot.pierces);
    assert_eq!(dot.final_amount, 3);

    // No longer derives Effect::Damage — cascade (GainRage×2) is derived directly.
    let rage_derived = derived
        .iter()
        .filter(|e| matches!(e, Effect::GainRage { .. }))
        .count();
    assert_eq!(
        rage_derived, 2,
        "should derive GainRage for source and target"
    );
}

/// hp_percent_dot uses ceil division: ceil(7 * 10 / 100) = 1.
#[test]
fn tick_dot_with_percent_dot_uses_ceil() {
    let mut target = make_unit(1, 7, 7);
    target.statuses.push(status_with_dot("burning", 0, 2));
    let mut state = state_with(vec![target]);

    let (_derived, ctx) = apply_effect(
        &mut state,
        &Effect::TickDot {
            target: UnitId(1),
            status: StatusId::from("burning"),
        },
        &StubContent::with_hp_percent_dot(10), // 10% of 7 = 0.7 → ceil = 1
    );

    // HP reduced by 1 (ceil of 10% of 7).
    assert_eq!(
        state.unit(UnitId(1)).unwrap().hp(),
        6,
        "HP should be reduced by 1 (ceil)"
    );

    let dot = ctx
        .dot_damage
        .as_ref()
        .expect("dot_damage must be populated");
    assert_eq!(dot.final_amount, 1);
    assert!((dot.raw - 1.0).abs() < f32::EPSILON);
    assert!(dot.pierces);
    assert_eq!(dot.mitigation, 0);
}

/// Both dot_per_tick > 0 and hp_percent_dot > 0 fuse into a single DotDamaged ctx
/// with the combined raw total (flat + percent-ceil).
#[test]
fn tick_dot_with_both_dot_and_percent_returns_two_damages() {
    let mut target = make_unit(1, 10, 10);
    target.statuses.push(status_with_dot("poison", 2, 2));
    let mut state = state_with(vec![target]);

    // hp_percent_dot=20% of max_hp=10 → ceil(10*20/100) = 2; flat = 2 → total = 4
    let (_derived, ctx) = apply_effect(
        &mut state,
        &Effect::TickDot {
            target: UnitId(1),
            status: StatusId::from("poison"),
        },
        &StubContent::with_hp_percent_dot(20),
    );

    // HP reduced by flat(2) + percent(2) = 4.
    assert_eq!(
        state.unit(UnitId(1)).unwrap().hp(),
        6,
        "HP should be reduced by 4 (2 flat + 2 percent)"
    );

    // Single fused DotDamageCtx with total raw.
    let dot = ctx
        .dot_damage
        .as_ref()
        .expect("dot_damage must be populated");
    assert!(
        (dot.raw - 4.0).abs() < f32::EPSILON,
        "raw should be sum of flat + percent"
    );
    assert_eq!(dot.final_amount, 4);
    assert!(dot.pierces);
}

/// No status on target → silent no-op.
#[test]
fn tick_dot_silent_when_status_absent() {
    let target = make_unit(1, 10, 10); // no statuses
    let mut state = state_with(vec![target]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::TickDot {
            target: UnitId(1),
            status: StatusId::from("poison"),
        },
        &StubContent::neutral(),
    );

    assert!(derived.is_empty());
}

/// Unknown UnitId → silent no-op.
#[test]
fn tick_dot_silent_when_target_missing() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::TickDot {
            target: UnitId(99),
            status: StatusId::from("poison"),
        },
        &StubContent::neutral(),
    );

    assert!(derived.is_empty());
}

/// `effect_to_event(TickDot)` with empty ctx (zero-damage tick) returns `StatusTicked`.
#[test]
fn tick_dot_emits_status_ticked_event() {
    let mut target = make_unit(1, 10, 10);
    target.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 2,
        dot_per_tick: 3,
        applier: EffectSource::Unit(UnitId(42)),
    });
    let state = state_with(vec![target]);

    // Passing ApplyCtx::default() simulates a zero-damage tick (dot_damage = None).
    let effect = Effect::TickDot {
        target: UnitId(1),
        status: StatusId::from("poison"),
    };
    let event = effect_to_event(&effect, &state, None, &ApplyCtx::default());

    assert!(matches!(
        event,
        Some(Event::StatusTicked {
            target: UnitId(1),
            source: EffectSource::Unit(UnitId(42)),
            ref status,
        }) if status == &StatusId::from("poison")
    ));
}

/// `effect_to_event(TickDot)` with a populated `dot_damage` ctx returns `DotDamaged`.
#[test]
fn tick_dot_emits_dot_damaged_event_when_damage_present() {
    use combat_engine::effect::DotDamageCtx;

    let target = make_unit(1, 10, 10);
    let state = state_with(vec![target]);

    let effect = Effect::TickDot {
        target: UnitId(1),
        status: StatusId::from("poison"),
    };
    let ctx = ApplyCtx {
        dot_damage: Some(DotDamageCtx {
            source: EffectSource::Unit(UnitId(42)),
            source_status: StatusId::from("poison"),
            raw: 3.0,
            mitigation: 0,
            pierces: true,
            final_amount: 3,
        }),
        ..ApplyCtx::default()
    };
    let event = effect_to_event(&effect, &state, None, &ctx);

    assert!(matches!(
        event,
        Some(Event::DotDamaged {
            target: UnitId(1),
            source: EffectSource::Unit(UnitId(42)),
            amount: 3,
            pierces: true,
            mitigation: 0,
            ..
        })
    ));
}

// ── ExpireStatus ──────────────────────────────────────────────────────────────

/// rounds_remaining > 1 → decremented, no removal, no derived.
#[test]
fn expire_status_decrements_rounds() {
    let mut u = make_unit(1, 10, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 3,
        dot_per_tick: 2,
        applier: EffectSource::Unit(UnitId(2)),
    });
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::ExpireStatus {
            target: UnitId(1),
            status: StatusId::from("poison"),
        },
        &StubContent::neutral(),
    );

    let unit = state.unit(UnitId(1)).unwrap();
    assert_eq!(unit.statuses.len(), 1, "status still present");
    assert_eq!(unit.statuses[0].rounds_remaining, 2);
    assert!(derived.is_empty(), "no removal → no RefreshAggregates");
}

/// rounds_remaining == 1 → decremented to 0, derives RemoveStatus.
/// Cascading RemoveStatus removes the status and derives RefreshAggregates.
#[test]
fn expire_status_removes_at_zero() {
    let mut u = make_unit(1, 10, 10);
    u.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 1,
        dot_per_tick: 2,
        applier: EffectSource::Unit(UnitId(2)),
    });
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::ExpireStatus {
            target: UnitId(1),
            status: StatusId::from("poison"),
        },
        &StubContent::neutral(),
    );

    // Status still in list (rounds_remaining == 0), removal deferred to cascade.
    assert_eq!(state.unit(UnitId(1)).unwrap().statuses.len(), 1);
    assert_eq!(
        state.unit(UnitId(1)).unwrap().statuses[0].rounds_remaining,
        0
    );
    assert_eq!(derived.len(), 1);
    assert!(
        matches!(&derived[0], Effect::RemoveStatus { target: UnitId(1), status } if status == &StatusId::from("poison"))
    );

    // Cascade: RemoveStatus removes the status and derives RefreshAggregates.
    let (refresh, _) = apply_effect(&mut state, &derived[0], &StubContent::neutral());
    assert!(
        state.unit(UnitId(1)).unwrap().statuses.is_empty(),
        "status removed after cascade"
    );
    assert_eq!(refresh.len(), 1);
    assert!(matches!(
        refresh[0],
        Effect::RefreshAggregates { unit: UnitId(1) }
    ));
}

/// Status not present on unit → silent no-op.
#[test]
fn expire_status_silent_when_absent() {
    let u = make_unit(1, 10, 10);
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::ExpireStatus {
            target: UnitId(1),
            status: StatusId::from("nonexistent"),
        },
        &StubContent::neutral(),
    );

    assert!(derived.is_empty());
    assert!(state.unit(UnitId(1)).unwrap().statuses.is_empty());
}

// ── Death (status cleanup) ────────────────────────────────────────────────────

/// Death derives one RemoveStatus per unique status id on the dying unit, in
/// insertion order. After cascading, statuses is empty and each RemoveStatus
/// derived one RefreshAggregates.
#[test]
fn death_derives_remove_status_per_local_status() {
    let mut u = make_unit(1, 0, 20);
    u.statuses = vec![
        ActiveStatus {
            id: StatusId::from("burning"),
            rounds_remaining: 2,
            dot_per_tick: 3,
            applier: EffectSource::Unit(UnitId(2)),
        },
        ActiveStatus {
            id: StatusId::from("stunned"),
            rounds_remaining: 2,
            dot_per_tick: 0,
            applier: EffectSource::Unit(UnitId(3)),
        },
    ];
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Death { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    // One RemoveStatus per distinct id, in insertion order.
    assert_eq!(derived.len(), 2);
    assert!(
        matches!(&derived[0], Effect::RemoveStatus { target: UnitId(1), status } if status == &StatusId::from("burning"))
    );
    assert!(
        matches!(&derived[1], Effect::RemoveStatus { target: UnitId(1), status } if status == &StatusId::from("stunned"))
    );

    // Cascade: apply each derived effect; each should derive one RefreshAggregates.
    for eff in &derived {
        let (refresh, _) = apply_effect(&mut state, eff, &StubContent::neutral());
        assert_eq!(refresh.len(), 1);
        assert!(matches!(
            refresh[0],
            Effect::RefreshAggregates { unit: UnitId(1) }
        ));
    }
    assert!(state.unit(UnitId(1)).unwrap().statuses.is_empty());
}

/// If a unit somehow has two entries with the same status id, Death deduplicates
/// to one RemoveStatus (which removes all matching entries anyway).
#[test]
fn death_with_duplicate_status_ids_dedups_to_one_remove() {
    let mut u = make_unit(1, 0, 20);
    u.statuses = vec![
        ActiveStatus {
            id: StatusId::from("poison"),
            rounds_remaining: 3,
            dot_per_tick: 2,
            applier: EffectSource::Unit(UnitId(2)),
        },
        ActiveStatus {
            id: StatusId::from("poison"),
            rounds_remaining: 1,
            dot_per_tick: 1,
            applier: EffectSource::Unit(UnitId(3)),
        },
    ];
    let mut state = state_with(vec![u]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Death { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    assert_eq!(derived.len(), 1, "deduped to one RemoveStatus");
    assert!(
        matches!(&derived[0], Effect::RemoveStatus { target: UnitId(1), status } if status == &StatusId::from("poison"))
    );

    // Applying it removes both entries.
    apply_effect(&mut state, &derived[0], &StubContent::neutral());
    assert!(state.unit(UnitId(1)).unwrap().statuses.is_empty());
}

/// Death only touches the dying unit's own status list; sirota-style statuses
/// applied by the dying unit onto other units are unaffected.
#[test]
fn death_does_not_touch_sirota_statuses_on_other_units() {
    let mut unit_a = make_unit(1, 0, 20);
    unit_a.statuses = vec![ActiveStatus {
        id: StatusId::from("poisoned"),
        rounds_remaining: 2,
        dot_per_tick: 1,
        applier: EffectSource::Unit(UnitId(99)),
    }];
    let mut unit_b = make_unit(2, 20, 20);
    // cursed applied BY unit A onto B (applier == A).
    unit_b.statuses = vec![ActiveStatus {
        id: StatusId::from("cursed"),
        rounds_remaining: 3,
        dot_per_tick: 0,
        applier: EffectSource::Unit(UnitId(1)),
    }];
    let mut state = state_with(vec![unit_a, unit_b]);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Death { unit: UnitId(1) },
        &StubContent::neutral(),
    );

    // Only A's own status is targeted.
    assert_eq!(derived.len(), 1);
    assert!(
        matches!(&derived[0], Effect::RemoveStatus { target: UnitId(1), status } if status == &StatusId::from("poisoned"))
    );

    // B's cursed status is untouched.
    let b = state.unit(UnitId(2)).unwrap();
    assert_eq!(b.statuses.len(), 1);
    assert_eq!(b.statuses[0].id, StatusId::from("cursed"));
}

/// Full cascade via Damage: Damage → Death → RemoveStatus → RefreshAggregates.
/// Unit ends up dead with no statuses.
#[test]
fn death_cascade_from_damage_clears_statuses() {
    let mut target = make_unit(1, 1, 20);
    target.statuses = vec![ActiveStatus {
        id: StatusId::from("haste"),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: EffectSource::Unit(UnitId(2)),
    }];
    let source = make_unit(2, 20, 20);
    let mut state = state_with(vec![target, source]);
    let content = StubContent::neutral();

    // Apply lethal Damage manually and drain the derived queue.
    let (mut queue, _) = apply_effect(
        &mut state,
        &Effect::Damage {
            target: UnitId(1),
            raw: 100.0,
            source: EffectSource::Unit(UnitId(2)),
            pierces: true,
            magic: false,
        },
        &content,
    );
    while let Some(eff) = queue.first().cloned() {
        queue.remove(0);
        let (more, _) = apply_effect(&mut state, &eff, &content);
        queue.extend(more);
    }

    assert_eq!(state.unit(UnitId(1)).unwrap().hp(), 0);
    assert!(state.unit(UnitId(1)).unwrap().statuses.is_empty());
}

// ── Spawn (step 3.5a) ─────────────────────────────────────────────────────────

use storyforge::combat_engine::effect::SpawnBlockedReason;
use storyforge::combat_engine::UnitTemplate;

fn test_template() -> UnitTemplate {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    UnitTemplate {
        max_hp: 8,
        armor: 1,
        magic_resist: 0,
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
        tags: Default::default(),
    }
}

#[test]
fn spawn_creates_unit_with_correct_template_stats() {
    use storyforge::combat_engine::PoolKind;
    let summoner = make_unit(1, 20, 20);
    let summoner_pos = summoner.pos;
    let summoner_team = summoner.team;
    let mut state = state_with(vec![summoner]);
    let before = state.units().len();
    let content = StubContent::neutral().with_template("imp", test_template());

    let (derived, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: None,
        },
        &content,
    );

    assert!(derived.is_empty());
    assert_eq!(state.units().len(), before + 1);
    assert!(ctx.spawn_blocked.is_none());
    let uid = ctx.spawn_uid.expect("spawn_uid set on success");
    let pos = ctx.spawn_pos.expect("spawn_pos set on success");
    let spawned = state.unit(uid).expect("new unit present");
    assert_eq!(spawned.hp(), 8);
    assert_eq!(spawned.max_hp(), 8);
    assert_eq!(spawned.runtime.armor, 1);
    assert_eq!(spawned.runtime.base_speed, 4);
    // max_ap is encoded in pools[Ap]
    assert_eq!(
        spawned.pools[PoolKind::Ap].map(|(_, max)| max).unwrap_or(0),
        1
    );
    assert_eq!(spawned.team, summoner_team);
    assert_eq!(spawned.summoner, Some(UnitId(1)));
    assert_eq!(spawned.pos, pos);
    assert_ne!(spawned.pos, summoner_pos);
    assert!(summoner_pos.distance_to(spawned.pos) <= 2);
}

/// F2 follow-up: mid-combat `Effect::Spawn` must apply `template.initial_statuses`
/// to the freshly summoned unit, mirroring the bootstrap path. This was a
/// known gap until shared helper `apply_template_initial_statuses` was wired in.
#[test]
fn spawn_applies_initial_statuses_to_summoned_unit() {
    use storyforge::combat_engine::{StatusId, PERMANENT_DURATION};

    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);

    // Template with a permanent stun in initial_statuses.
    let mut tpl = test_template();
    tpl.initial_statuses = vec![StatusId::from("stunned")];
    let content = StubContent::neutral().with_template("frozen_imp", tpl);

    let (_derived, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "frozen_imp".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn_uid set on success");
    let spawned = state.unit(uid).expect("new unit present");
    let status = spawned
        .statuses
        .iter()
        .find(|s| s.id == StatusId::from("stunned"))
        .expect("summoned unit must carry initial status 'stunned'");
    assert_eq!(
        status.rounds_remaining, PERMANENT_DURATION,
        "initial status must be permanent (sentinel duration)"
    );
    assert_eq!(
        spawned.template_id.as_deref(),
        Some("frozen_imp"),
        "spawned unit must record template_id for future apply paths"
    );
}

/// `initial_pools[Hp]` override: template with max_hp=10 and initial_pools[hp]=5
/// must spawn a unit with hp()=5, max_hp()=10.
#[test]
fn spawn_respects_initial_pools_override() {
    use storyforge::combat_engine::PoolKind;

    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);

    let mut tpl = test_template();
    tpl.max_hp = 10;
    tpl.initial_pools[PoolKind::Hp] = Some(5);
    let content = StubContent::neutral().with_template("wounded", tpl);

    let (_derived, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "wounded".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn_uid set on success");
    let spawned = state.unit(uid).expect("new unit present");
    assert_eq!(spawned.hp(), 5, "initial_pools[hp]=5 must be applied");
    assert_eq!(spawned.max_hp(), 10, "max_hp must remain 10");
}

/// `initial_pools[Hp]` clamp: value > max_hp must be clamped to max_hp.
#[test]
fn spawn_clamps_initial_pools_to_max() {
    use storyforge::combat_engine::PoolKind;

    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);

    let mut tpl = test_template();
    tpl.max_hp = 10;
    tpl.initial_pools[PoolKind::Hp] = Some(999);
    let content = StubContent::neutral().with_template("overheal", tpl);

    let (_derived, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "overheal".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn_uid set on success");
    let spawned = state.unit(uid).expect("new unit present");
    assert_eq!(
        spawned.hp(),
        10,
        "initial_pools value must be clamped to max_hp"
    );
    assert_eq!(spawned.max_hp(), 10);
}

#[test]
fn spawn_blocked_when_template_missing() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    let before = state.units().len();
    let content = StubContent::neutral();

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "missing".into(),
            max_active: None,
        },
        &content,
    );

    assert_eq!(state.units().len(), before);
    assert_eq!(ctx.spawn_blocked, Some(SpawnBlockedReason::TemplateMissing));
    assert!(ctx.spawn_uid.is_none());
}

#[test]
fn spawn_blocked_at_max_active_cap() {
    let summoner = make_unit(1, 20, 20);
    let mut minion_a = make_unit(2, 5, 5);
    minion_a.summoner = Some(UnitId(1));
    minion_a.pos = hex_from_offset(1, 0);
    let mut minion_b = make_unit(3, 5, 5);
    minion_b.summoner = Some(UnitId(1));
    minion_b.pos = hex_from_offset(0, 1);
    let mut state = state_with(vec![summoner, minion_a, minion_b]);
    let before = state.units().len();
    let content = StubContent::neutral().with_template("imp", test_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: Some(2),
        },
        &content,
    );

    assert_eq!(state.units().len(), before);
    assert_eq!(
        ctx.spawn_blocked,
        Some(SpawnBlockedReason::MaxActiveReached)
    );
}

#[test]
fn spawn_blocked_when_no_free_position() {
    let summoner = make_unit(1, 20, 20);
    let summoner_pos = summoner.pos;
    // Fill every cell in radius 2 around summoner (excluding summoner's own).
    let mut units = vec![summoner];
    let mut next_id: u64 = 100;
    for cell in summoner_pos.range(2) {
        if cell == summoner_pos {
            continue;
        }
        let mut blocker = make_unit(next_id, 5, 5);
        blocker.pos = cell;
        units.push(blocker);
        next_id += 1;
    }
    let mut state = state_with(units);
    let before = state.units().len();
    let content = StubContent::neutral().with_template("imp", test_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: None,
        },
        &content,
    );

    assert_eq!(state.units().len(), before);
    assert_eq!(ctx.spawn_blocked, Some(SpawnBlockedReason::NoFreePosition));
}

/// Regression: corpse tombstones must block spawn positions.
///
/// The engine and the Bevy `HexPositions` map must agree on what cells are
/// occupied.  ECS keeps `HexPositions` entries for dead entities until they
/// are despawned, so spawning a new unit on a corpse would panic in
/// `HexPositions::insert`.  Engine occupancy uses `state.units()` (all units,
/// including dead) to match this view.
#[test]
fn spawn_blocked_by_corpse_tombstone() {
    let summoner = make_unit(1, 20, 20);
    let summoner_pos = summoner.pos;
    // Fill ring(2) with corpses (hp=0). Engine must treat them as obstacles.
    let mut units = vec![summoner];
    let mut next_id: u64 = 100;
    for cell in summoner_pos.range(2) {
        if cell == summoner_pos {
            continue;
        }
        let mut corpse = make_unit(next_id, 0, 5);
        corpse.pos = cell;
        units.push(corpse);
        next_id += 1;
    }
    let mut state = state_with(units);
    let content = StubContent::neutral().with_template("imp", test_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: None,
        },
        &content,
    );

    assert_eq!(
        ctx.spawn_blocked,
        Some(SpawnBlockedReason::NoFreePosition),
        "corpse tombstones must block spawn cells"
    );
}

#[test]
fn spawn_synthetic_uid_above_bevy_bit_range() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    let content = StubContent::neutral().with_template("imp", test_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("success");
    assert!(
        uid.0 >= 1u64 << 63,
        "synthetic UID must avoid Bevy Entity::to_bits() range"
    );
}

#[test]
fn effect_to_event_emits_unit_spawned_on_success() {
    let summoner = make_unit(1, 20, 20);
    let summoner_team = summoner.team;
    let mut state = state_with(vec![summoner]);
    let content = StubContent::neutral().with_template("imp", test_template());

    let effect = Effect::Spawn {
        summoner: UnitId(1),
        template_id: "imp".into(),
        max_active: None,
    };
    let (_, ctx) = apply_effect(&mut state, &effect, &content);

    let ev = effect_to_event(&effect, &state, None, &ctx).expect("UnitSpawned event on success");
    match ev {
        Event::UnitSpawned {
            uid,
            summoner,
            pos,
            template_id,
            team,
        } => {
            assert_eq!(uid, ctx.spawn_uid.unwrap());
            assert_eq!(summoner, UnitId(1));
            assert_eq!(pos, ctx.spawn_pos.unwrap());
            assert_eq!(template_id, "imp");
            assert_eq!(team, summoner_team);
        }
        other => panic!("expected UnitSpawned, got {:?}", other),
    }
}

#[test]
fn effect_to_event_emits_spawn_blocked_on_failure() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    let content = StubContent::neutral();

    let effect = Effect::Spawn {
        summoner: UnitId(1),
        template_id: "missing".into(),
        max_active: None,
    };
    let (_, ctx) = apply_effect(&mut state, &effect, &content);

    let ev = effect_to_event(&effect, &state, None, &ctx).expect("SpawnBlocked event on failure");
    match ev {
        Event::SpawnBlocked {
            summoner,
            template_id,
            reason,
        } => {
            assert_eq!(summoner, UnitId(1));
            assert_eq!(template_id, "missing");
            assert_eq!(reason, SpawnBlockedReason::TemplateMissing);
        }
        other => panic!("expected SpawnBlocked, got {:?}", other),
    }
}

// ── Spawn caster_context / aoo_dice propagation (step 3.7-I) ─────────────────

use storyforge::combat_engine::dice::DiceExpr;
use storyforge::combat_engine::{CasterContext, CritFailOutcome};

/// Template with a non-trivial CasterContext (str_mod=3, weapon_dice=2d6).
fn melee_template() -> UnitTemplate {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    let weapon_dice = DiceExpr::new(2, 6, 0);
    UnitTemplate {
        max_hp: 10,
        armor: 2,
        magic_resist: 0,
        base_speed: 3,
        max_ap: 1,
        mana_max: 0,
        energy_max: 0,
        rage_max: 0,
        caster_context: CasterContext {
            str_mod: 3,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: Some(weapon_dice),
            crit_fail_outcome: CritFailOutcome::Miss,
            dex_mod: 0,
            ranged_dice: None,
        },
        aoo_dice: Some(DiceExpr::new(2, 6, 3)), // weapon + str_mod baked in
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
        tags: Default::default(),
    }
}

#[test]
fn spawn_unit_carries_caster_context_from_template() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    let content = StubContent::neutral().with_template("warrior", melee_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "warrior".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn succeeded");
    let spawned = state.unit(uid).expect("spawned unit present");
    assert_eq!(
        spawned.caster_context.str_mod, 3,
        "str_mod carried from template"
    );
    assert_eq!(
        spawned.caster_context.weapon_dice,
        Some(DiceExpr::new(2, 6, 0)),
        "weapon_dice carried from template"
    );
}

#[test]
fn spawn_unit_carries_aoo_dice_from_template() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    let content = StubContent::neutral().with_template("warrior", melee_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "warrior".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn succeeded");
    let spawned = state.unit(uid).expect("spawned unit present");
    assert!(
        spawned.aoo_dice.is_some(),
        "melee template should have aoo_dice"
    );
    assert_eq!(
        spawned.aoo_dice,
        Some(DiceExpr::new(2, 6, 3)),
        "aoo_dice carried from template"
    );
}

#[test]
fn spawn_unit_has_default_caster_when_template_default() {
    let summoner = make_unit(1, 20, 20);
    let mut state = state_with(vec![summoner]);
    // test_template() has default (zero) CasterContext and None aoo_dice.
    let content = StubContent::neutral().with_template("imp", test_template());

    let (_, ctx) = apply_effect(
        &mut state,
        &Effect::Spawn {
            summoner: UnitId(1),
            template_id: "imp".into(),
            max_active: None,
        },
        &content,
    );

    let uid = ctx.spawn_uid.expect("spawn succeeded");
    let spawned = state.unit(uid).expect("spawned unit present");
    assert_eq!(
        spawned.caster_context,
        CasterContext::default(),
        "default context carried"
    );
    assert!(
        spawned.aoo_dice.is_none(),
        "no aoo_dice for default template"
    );
    assert!(spawned.auras.is_empty(), "no auras for default template");
    assert!(
        spawned.enemy_phases.is_empty(),
        "no phases for default template"
    );
}
