//! Tests for `check_legality` — LOS branch (T1.2.3).
//!
//! Validates that `NoLineOfSight` is returned when `requires_los=true` and
//! `is_blocked_los` signals obstruction, and that LOS checks are skipped
//! for melee (range ≤ 1) or when `requires_los=false`.

use combat_engine::{
    AbilityId, StatusDef, StatusId,
    content::{AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, TargetType, TeamRelation},
    legality::{check_legality, ActionState, ActorView, IllegalReason, ProposedAction},
    state::{Team, UnitId},
};
use hexx::Hex;

// ── Minimal content stub ──────────────────────────────────────────────────────

struct NoContent;
impl ContentView for NoContent {
    fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<&combat_engine::content::UnitTemplate> { None }
    fn status_bonuses(&self, _: &StatusId) -> combat_engine::content::StatusBonuses {
        combat_engine::content::StatusBonuses::default()
    }
}

// ── Test ActionState stub ─────────────────────────────────────────────────────

/// A minimal ActionState for legality tests.
/// `blocked_los` controls whether `is_blocked_los` returns true.
struct TestState {
    actor_pos: Hex,
    target_pos: Hex,
    ability: AbilityDef,
    blocked_los: bool,
}

impl ActionState for TestState {
    type Id = UnitId;

    fn ability_def(&self, _id: &AbilityId) -> Option<AbilityDef> {
        Some(self.ability.clone())
    }

    fn status_def(&self, _id: &StatusId) -> Option<StatusDef> { None }

    fn actor_view(&self, actor: Self::Id) -> Option<ActorView> {
        if actor == UnitId(1) {
            Some(ActorView {
                pos: self.actor_pos,
                team: Team::Player,
                hp: 10,
                ap: 5,
                pools: combat_engine::enum_map::enum_map! {
                    combat_engine::PoolKind::Hp     => None,
                    combat_engine::PoolKind::Mana   => None,
                    combat_engine::PoolKind::Rage   => None,
                    combat_engine::PoolKind::Energy => None,
                    combat_engine::PoolKind::Ap     => None,
                    combat_engine::PoolKind::Mp     => None,
                },
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                is_alive: true,
            })
        } else {
            None
        }
    }

    fn actor_knows_ability(&self, _actor: Self::Id, _ability: &AbilityId) -> bool { true }

    fn is_target_alive(&self, target: Self::Id) -> Option<bool> {
        if target == UnitId(2) { Some(true) } else { None }
    }

    fn target_team(&self, target: Self::Id) -> Option<Team> {
        if target == UnitId(2) { Some(Team::Enemy) } else { None }
    }

    fn taunters_for(&self, _actor_team: Team) -> Vec<Self::Id> { vec![] }

    fn is_in_bounds(&self, _pos: Hex) -> bool { true }

    fn is_blocked_los(&self, _from: Hex, _to: Hex) -> bool {
        self.blocked_los
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ranged_ability(requires_los: bool) -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::None,
        statuses: vec![],
        requires_los,
    }
}

fn melee_ability() -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 1 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        effect: EffectDef::None,
        statuses: vec![],
        requires_los: false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Ranged ability with requires_los=true + blocked LOS → NoLineOfSight.
#[test]
fn legality_blocks_ranged_attack_through_obstacle() {
    let state = TestState {
        actor_pos: Hex::new(0, 0),
        target_pos: Hex::new(3, 0),
        ability: ranged_ability(true),
        blocked_los: true,
    };
    let result = check_legality(
        ProposedAction {
            actor: UnitId(1),
            ability: &AbilityId::from("bow_shot"),
            target: UnitId(2),
            target_pos: Hex::new(3, 0),
        },
        &state,
    );
    assert_eq!(result, Err(IllegalReason::NoLineOfSight));
}

/// Ranged ability with requires_los=true + clear LOS → Ok.
#[test]
fn legality_allows_ranged_attack_with_clear_los() {
    let state = TestState {
        actor_pos: Hex::new(0, 0),
        target_pos: Hex::new(3, 0),
        ability: ranged_ability(true),
        blocked_los: false,
    };
    let result = check_legality(
        ProposedAction {
            actor: UnitId(1),
            ability: &AbilityId::from("bow_shot"),
            target: UnitId(2),
            target_pos: Hex::new(3, 0),
        },
        &state,
    );
    assert!(result.is_ok(), "clear LOS must allow ranged attack: {result:?}");
}

/// Melee ability (range.max == 1) must never check LOS even if blocked.
#[test]
fn legality_skips_los_for_melee_range_1() {
    let state = TestState {
        actor_pos: Hex::new(0, 0),
        target_pos: Hex::new(1, 0),
        ability: melee_ability(),
        blocked_los: true,
    };
    let result = check_legality(
        ProposedAction {
            actor: UnitId(1),
            ability: &AbilityId::from("strike"),
            target: UnitId(2),
            target_pos: Hex::new(1, 0),
        },
        &state,
    );
    assert!(result.is_ok(), "melee must never produce NoLineOfSight: {result:?}");
}

/// Ranged ability with requires_los=false must not check LOS even if blocked.
#[test]
fn legality_skips_los_when_requires_los_false() {
    let state = TestState {
        actor_pos: Hex::new(0, 0),
        target_pos: Hex::new(3, 0),
        ability: ranged_ability(false),
        blocked_los: true,
    };
    let result = check_legality(
        ProposedAction {
            actor: UnitId(1),
            ability: &AbilityId::from("magic_bolt"),
            target: UnitId(2),
            target_pos: Hex::new(3, 0),
        },
        &state,
    );
    assert!(result.is_ok(), "requires_los=false must not trigger NoLineOfSight: {result:?}");
}

/// Default ActionState (without is_blocked_los override) returns false.
/// Documents that the default impl is safe (no LOS blocking by default).
#[test]
fn default_action_state_returns_no_los_blocking() {
    struct DefaultState;
    impl ActionState for DefaultState {
        type Id = UnitId;
        fn ability_def(&self, _: &AbilityId) -> Option<AbilityDef> {
            Some(ranged_ability(true))
        }
        fn status_def(&self, _: &StatusId) -> Option<StatusDef> { None }
        fn actor_view(&self, actor: Self::Id) -> Option<ActorView> {
            if actor == UnitId(1) {
                Some(ActorView {
                    pos: Hex::new(0, 0),
                    team: Team::Player,
                    hp: 10, ap: 5,
                    pools: combat_engine::enum_map::enum_map! {
                        combat_engine::PoolKind::Hp     => None,
                        combat_engine::PoolKind::Mana   => None,
                        combat_engine::PoolKind::Rage   => None,
                        combat_engine::PoolKind::Energy => None,
                        combat_engine::PoolKind::Ap     => None,
                        combat_engine::PoolKind::Mp     => None,
                    },
                    causes_disadvantage: false,
                    blocks_mana_abilities: false,
                    is_alive: true,
                })
            } else { None }
        }
        fn actor_knows_ability(&self, _: Self::Id, _: &AbilityId) -> bool { true }
        fn is_target_alive(&self, t: Self::Id) -> Option<bool> {
            if t == UnitId(2) { Some(true) } else { None }
        }
        fn target_team(&self, t: Self::Id) -> Option<Team> {
            if t == UnitId(2) { Some(Team::Enemy) } else { None }
        }
        fn taunters_for(&self, _: Team) -> Vec<Self::Id> { vec![] }
        fn is_in_bounds(&self, _: Hex) -> bool { true }
        // is_blocked_los NOT overridden — uses default (false)
    }

    let result = check_legality(
        ProposedAction {
            actor: UnitId(1),
            ability: &AbilityId::from("bow_shot"),
            target: UnitId(2),
            target_pos: Hex::new(3, 0),
        },
        &DefaultState,
    );
    // Default impl returns false → no LOS blocking → Ok
    assert!(result.is_ok(), "default is_blocked_los must not block actions: {result:?}");
}
