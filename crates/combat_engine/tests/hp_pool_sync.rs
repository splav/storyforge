//! Invariant tests for HP-as-pool Stage 1 dual-write safety net.
//!
//! Verifies that `unit.pools[PoolKind::Hp]` stays in sync with
//! `unit.hp` / `unit.max_hp` after every HP-mutating effect.
//!
//! These tests guard against dual-write gaps introduced during the Stage 1
//! migration; Stage 3 will remove the legacy fields entirely.

use hexx::Hex;

use combat_engine::{
    PoolKind, RegenRule, StatusBonuses, StatusId,
    content::ContentView,
    effect::{Effect, apply_effect},
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId {
    UnitId(n)
}

/// Build a unit at full HP with pools[Hp] correctly initialised.
fn make_unit(id: u64, hp: i32, max_hp: i32) -> Unit {
    Unit {
        id: uid(id),
        team: Team::Player,
        pos: Hex::ZERO,
        hp,
        max_hp,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 4,
        speed: 4,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: vec![],
        enemy_phases: vec![],
        pools: combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => Some((hp, max_hp)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((4, 4)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        template_id: None,
    }
}

/// Assert that `pools[Hp]` mirrors `hp`/`max_hp` on every unit in the state.
fn assert_all_hp_sync(state: &CombatState) {
    for u in state.units() {
        let pool = u.pools[PoolKind::Hp]
            .expect("pools[Hp] must be Some on every unit after Stage 1");
        assert_eq!(
            pool.0, u.hp,
            "unit {} pools[Hp].0 ({}) != hp ({}) after effect",
            u.id.0, pool.0, u.hp
        );
        assert_eq!(
            pool.1, u.max_hp,
            "unit {} pools[Hp].1 ({}) != max_hp ({}) after effect",
            u.id.0, pool.1, u.max_hp
        );
    }
}

struct NoContent;
impl ContentView for NoContent {
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses {
        StatusBonuses::default()
    }
    fn ability_def(
        &self,
        _: &combat_engine::AbilityId,
    ) -> Option<&combat_engine::AbilityDef> {
        None
    }
    fn status_def(&self, _: &StatusId) -> Option<&combat_engine::StatusDef> {
        None
    }
    fn unit_template(&self, _: &str) -> Option<combat_engine::UnitTemplate> {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn hp_pool_dual_write_invariant_after_damage() {
    let target = uid(1);
    let source = uid(2);
    let mut state = CombatState::new(
        vec![
            make_unit(1, 20, 20),
            make_unit(2, 20, 20),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.set_turn_queue(vec![target, source], 0);

    // Apply 5 raw damage (no armor → final = 5).
    apply_effect(
        &mut state,
        &Effect::Damage { target, raw: 5, source, pierces: false },
        &NoContent,
    );

    let u = state.unit(target).expect("target exists");
    assert_eq!(u.hp, 15, "hp after damage");
    assert_all_hp_sync(&state);
}

#[test]
fn hp_pool_dual_write_invariant_after_heal() {
    let target = uid(1);
    let source = uid(2);
    let mut state = CombatState::new(
        vec![
            make_unit(1, 5, 20),  // wounded: hp=5, max_hp=20
            make_unit(2, 20, 20),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.set_turn_queue(vec![target, source], 0);

    // Heal 8 HP (clamped at max 20 → hp becomes 13).
    apply_effect(
        &mut state,
        &Effect::Heal { target, amount: 8 },
        &NoContent,
    );

    let u = state.unit(target).expect("target exists");
    assert_eq!(u.hp, 13, "hp after heal");
    assert_all_hp_sync(&state);
}

#[test]
fn hp_pool_dual_write_invariant_after_death() {
    let target = uid(1);
    let source = uid(2);
    let mut state = CombatState::new(
        vec![
            make_unit(1, 3, 20),  // low hp, about to die
            make_unit(2, 20, 20),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.set_turn_queue(vec![source, target], 0);

    apply_effect(
        &mut state,
        &Effect::Death { unit: target },
        &NoContent,
    );

    let u = state.unit(target).expect("tombstone exists");
    assert_eq!(u.hp, 0, "hp after death");
    // max_hp must be preserved, current must be 0.
    let pool = u.pools[PoolKind::Hp].expect("pool[Hp] must be Some after death");
    assert_eq!(pool.0, 0, "pools[Hp].0 == 0 after death");
    assert_eq!(pool.1, 20, "pools[Hp].1 == max_hp after death");
    assert_all_hp_sync(&state);
}

#[test]
fn hp_pool_dual_write_invariant_after_set_max_hp() {
    let unit_id = uid(1);
    let mut state = CombatState::new(
        vec![make_unit(1, 20, 20)],
        1,
        RoundPhase::ActorTurn,
        0,
    );

    // Reduce max_hp to 15 — current hp (20) must clamp to 15.
    apply_effect(
        &mut state,
        &Effect::SetMaxHp { unit: unit_id, max_hp: 15 },
        &NoContent,
    );

    let u = state.unit(unit_id).expect("unit exists");
    assert_eq!(u.hp, 15, "hp clamped to new max");
    assert_eq!(u.max_hp, 15, "max_hp updated");
    assert_all_hp_sync(&state);
}
