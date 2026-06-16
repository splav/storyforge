//! Tests for `combat_engine::preview_action` (read-only dry-run, Phase C preview).
//!
//! Covers:
//! 1. **Purity** — state is byte-for-byte unchanged after `preview_action`.
//! 2. **Parity with real `step`** — events from preview == events from `step` under `ExpectedValue`.
//! 3. **Damage forecast** — returned events carry correct `UnitDamaged` amount.
//! 4. **Lethal detection** — `UnitDied` present iff hp ≤ expected damage.
//! 5. **No crit-fail in preview** — `ExpectedValue` rolls 11, never 1.
//! 6. **Illegal action** — error passthrough, state unchanged.

use storyforge::combat_engine::{
    action::{Action, ActionError},
    content::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType},
    dice::{DiceExpr, ExpectedValue},
    event::Event,
    legality::IllegalReason,
    preview::preview_action,
    state::{CombatState, RoundPhase, Team, UnitId},
    step::step,
    AbilityId, PoolKind,
};

use crate::common::engine_unit::{EngineUnitBuilder, StubContent};
use storyforge::game::hex::hex_from_offset;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_unit(
    id: u64,
    team: Team,
    pos_col: i32,
    pos_row: i32,
) -> storyforge::combat_engine::state::Unit {
    EngineUnitBuilder::new(id)
        .team(team)
        .pos(pos_col, pos_row)
        .hp_full(20)
        .ap(4, 4)
        .mp(6, 6)
        .build()
}

fn state_with(units: Vec<storyforge::combat_engine::state::Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::ActorTurn, 0)
}

/// A minimal single-enemy damage ability: 1d4 Damage, range 5, cost 1 AP.
fn damage_ability(dice: DiceExpr) -> AbilityDef {
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        effect: EffectDef::Damage { dice },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
        power: None,
    }
}

// ── 1. Purity ─────────────────────────────────────────────────────────────────

/// `preview_action` must not mutate the caller's `state`.
///
/// We assert on all observable fields of both units: hp, AP, statuses.
/// (`CombatState` does not implement `PartialEq`, so we use accessors.)
#[test]
fn preview_does_not_mutate_state() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);
    let state = state_with(vec![actor, target]);

    let content = StubContent::new().with_ability("strike", damage_ability(DiceExpr::new(1, 4, 0)));
    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let actor_hp_before = state.unit(UnitId(1)).unwrap().hp();
    let target_hp_before = state.unit(UnitId(2)).unwrap().hp();
    let actor_ap_before = state.unit(UnitId(1)).unwrap().pools[PoolKind::Ap]
        .map(|(c, _)| c)
        .unwrap_or(0);

    let result = preview_action(&state, action, &content);
    assert!(result.is_ok(), "preview should succeed");

    // State must be identical to before the call.
    assert_eq!(
        state.unit(UnitId(1)).unwrap().hp(),
        actor_hp_before,
        "actor hp unchanged"
    );
    assert_eq!(
        state.unit(UnitId(2)).unwrap().hp(),
        target_hp_before,
        "target hp unchanged"
    );
    assert_eq!(
        state.unit(UnitId(1)).unwrap().pools[PoolKind::Ap]
            .map(|(c, _)| c)
            .unwrap_or(0),
        actor_ap_before,
        "actor AP unchanged"
    );
    assert_eq!(
        state.unit(UnitId(1)).unwrap().statuses.len(),
        0,
        "actor statuses unchanged"
    );
    assert_eq!(
        state.unit(UnitId(2)).unwrap().statuses.len(),
        0,
        "target statuses unchanged"
    );
}

// ── 2. Parity with real step under ExpectedValue ──────────────────────────────

/// `preview_action` events must exactly match those from `step` run under `ExpectedValue`.
///
/// This pins that `preview_action` is literally `step + ExpectedValue` and cannot
/// silently diverge.
#[test]
fn preview_events_match_step_under_expected_value() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);

    let ability = damage_ability(DiceExpr::new(1, 6, 0));
    let content = StubContent::new().with_ability("strike", ability);

    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let state = state_with(vec![actor, target]);

    // Preview events.
    let preview_events = preview_action(&state, action.clone(), &content).expect("preview ok");

    // Real step events (on a fresh clone so state is same).
    let mut state_for_step = state.clone();
    let (step_events, _) =
        step(&mut state_for_step, action, &mut ExpectedValue, &content).expect("step ok");

    assert_eq!(
        preview_events, step_events,
        "preview events must match step-under-ExpectedValue events"
    );
}

// ── 3. Damage forecast correctness ───────────────────────────────────────────

/// `preview_action` returns `UnitDamaged` with the analytically correct amount.
///
/// 1d4 expected = round(2.5) = 3. str_mod = 0 (default). armor = 0.
/// → `amount` = 3.
#[test]
fn preview_damage_forecast_amount_correct() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0); // hp=20, armor=0

    let content = StubContent::new().with_ability("strike", damage_ability(DiceExpr::new(1, 4, 0)));
    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let state = state_with(vec![actor, target]);
    let events = preview_action(&state, action, &content).expect("preview ok");

    let damaged = events.iter().find_map(|e| {
        if let Event::UnitDamaged { target, amount, .. } = e {
            Some((*target, *amount))
        } else {
            None
        }
    });
    let (hit_target, amount) = damaged.expect("UnitDamaged event must be present");
    assert_eq!(hit_target, UnitId(2), "damage hits the target");
    // ExpectedValue: 1d4 → round(2.5) = 3; str_mod=0, armor=0 → amount=3.
    assert_eq!(amount, 3, "expected damage = round(1d4 mean) = 3");
}

// ── 4. Lethal detection ───────────────────────────────────────────────────────

/// When expected damage kills the target, `UnitDied` is present.
/// When hp is one above lethal, `UnitDied` is absent.
///
/// 1d4: expected amount = 3 (with str_mod=0, armor=0).
/// lethal threshold: hp ≤ 3 → dies; hp = 4 → survives.
#[test]
fn preview_lethal_detection() {
    let ability_dice = DiceExpr::new(1, 4, 0); // amount = 3 under ExpectedValue

    let actor = make_unit(1, Team::Player, 0, 0);
    let content = StubContent::new().with_ability("strike", damage_ability(ability_dice));
    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    // Case A: hp=3 (exactly lethal) → UnitDied present.
    let mut dying_target = make_unit(2, Team::Enemy, 1, 0);
    dying_target.pools[PoolKind::Hp] = Some((3, 20));
    let state_dying = state_with(vec![actor.clone(), dying_target]);

    let events = preview_action(&state_dying, action.clone(), &content).expect("preview ok");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::UnitDied { unit: UnitId(2) })),
        "UnitDied must be present when hp == expected damage; events: {events:?}"
    );

    // Case B: hp=4 (one above lethal) → UnitDied absent.
    let mut surviving_target = make_unit(2, Team::Enemy, 1, 0);
    surviving_target.pools[PoolKind::Hp] = Some((4, 20));
    let state_surviving = state_with(vec![actor, surviving_target]);

    let events = preview_action(&state_surviving, action, &content).expect("preview ok");
    assert!(
        !events.iter().any(|e| matches!(e, Event::UnitDied { .. })),
        "UnitDied must NOT be present when hp > expected damage; events: {events:?}"
    );
}

// ── 5. No crit-fail in preview ────────────────────────────────────────────────

/// `preview_action` must never emit `Event::CritFailed`.
///
/// `ExpectedValue` rolls 11 on 1d20, so the crit-fail branch (roll == 1)
/// is structurally unreachable.
#[test]
fn preview_never_emits_crit_failed() {
    use storyforge::combat_engine::{content::CritFailOutcome, CasterContext};

    let mut actor = make_unit(1, Team::Player, 0, 0);
    // Arm the actor with a crit-fail outcome so we'd see CritFailed if it fires.
    actor.caster_context = CasterContext {
        crit_fail_outcome: CritFailOutcome::Miss,
        ..Default::default()
    };
    let target = make_unit(2, Team::Enemy, 1, 0);

    let content = StubContent::new().with_ability("strike", damage_ability(DiceExpr::new(1, 4, 0)));
    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("strike"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let state = state_with(vec![actor, target]);
    let events = preview_action(&state, action, &content).expect("preview ok");

    assert!(
        !events.iter().any(|e| matches!(e, Event::CritFailed { .. })),
        "CritFailed must never appear in a preview (ExpectedValue rolls 11, not 1); events: {events:?}"
    );
}

// ── 6. Illegal action ─────────────────────────────────────────────────────────

/// `preview_action` propagates `Err(ActionError::Illegal(...))` for unknown ability.
/// The original `state` must be unchanged.
#[test]
fn preview_illegal_action_returns_err_and_state_unchanged() {
    let actor = make_unit(1, Team::Player, 0, 0);
    let target = make_unit(2, Team::Enemy, 1, 0);
    let state = state_with(vec![actor, target]);

    let content = StubContent::new(); // no abilities registered
    let action = Action::Cast {
        actor: UnitId(1),
        ability: AbilityId::from("nonexistent"),
        target: UnitId(2),
        target_pos: hex_from_offset(1, 0),
    };

    let actor_hp_before = state.unit(UnitId(1)).unwrap().hp();
    let target_hp_before = state.unit(UnitId(2)).unwrap().hp();

    let err =
        preview_action(&state, action, &content).expect_err("unknown ability must be rejected");
    assert_eq!(err, ActionError::Illegal(IllegalReason::UnknownAbility));

    // State unchanged.
    assert_eq!(
        state.unit(UnitId(1)).unwrap().hp(),
        actor_hp_before,
        "actor hp after error"
    );
    assert_eq!(
        state.unit(UnitId(2)).unwrap().hp(),
        target_hp_before,
        "target hp after error"
    );
}
