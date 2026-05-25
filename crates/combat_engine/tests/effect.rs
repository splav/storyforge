//! Unit tests for `apply_effect` — focusing on B5: death of the current actor
//! must derive `Effect::AdvanceTurn` and push `Event::TurnEnded` via
//! `ctx.turn_skip_events`.

use hexx::Hex;

use combat_engine::{
    StatusBonuses, StatusId,
    content::ContentView,
    effect::{Effect, apply_effect},
    event::Event,
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId {
    UnitId(n)
}

fn make_unit(id: u64, team: Team, hp: i32, pos: Hex) -> Unit {
    use combat_engine::{PoolKind, RegenRule};
    Unit {
        id: uid(id),
        team,
        pos,
        hp,
        max_hp: hp,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 3,
        speed: 3,
        action_points: 2,
        max_ap: 2,
        movement_points: 3,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        rage: None,
        mana: None,
        energy: None,
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: vec![],
        enemy_phases: vec![],
        pools: combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((3, 3)),
        },
        regen_per_pool: combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
    }
}

struct NoContent;
impl ContentView for NoContent {
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses {
        StatusBonuses::default()
    }
    fn ability_def(&self, _: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
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

/// B5: When `Effect::Death` fires on the **current** turn actor, the derived
/// effects must include `Effect::AdvanceTurn` and `ctx.turn_skip_events` must
/// carry `Event::TurnEnded` for that actor.
#[test]
fn death_of_current_actor_derives_advance_turn_and_emits_turn_ended() {
    // Queue [A=1, B=2], current = A (index 0).
    let mut state = CombatState::new(
        vec![
            make_unit(1, Team::Player, 10, Hex::ZERO),
            make_unit(2, Team::Enemy, 10, Hex::new(2, 0)),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.set_turn_queue(vec![uid(1), uid(2)], 0);
    assert_eq!(state.turn_queue.current(), Some(uid(1)));

    let (derived, ctx) = apply_effect(&mut state, &Effect::Death { unit: uid(1) }, &NoContent);

    // AdvanceTurn must be in the derived effects.
    assert!(
        derived.iter().any(|e| matches!(e, Effect::AdvanceTurn)),
        "Expected Effect::AdvanceTurn in derived for current-actor death; got: {:?}",
        derived
    );

    // TurnEnded for the dying actor must be in turn_skip_events.
    assert!(
        ctx.turn_skip_events
            .iter()
            .any(|e| matches!(e, Event::TurnEnded { actor, .. } if *actor == uid(1))),
        "Expected Event::TurnEnded {{ actor: uid(1) }} in ctx.turn_skip_events; got: {:?}",
        ctx.turn_skip_events
    );
}

/// B5 non-regression: When `Effect::Death` fires on a **non-current** actor,
/// no `Effect::AdvanceTurn` is derived and `ctx.turn_skip_events` stays empty.
#[test]
fn death_of_non_current_actor_does_not_derive_advance_turn() {
    // Queue [A=1, B=2], current = A (index 0). B dies.
    let mut state = CombatState::new(
        vec![
            make_unit(1, Team::Player, 10, Hex::ZERO),
            make_unit(2, Team::Enemy, 10, Hex::new(2, 0)),
        ],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    state.set_turn_queue(vec![uid(1), uid(2)], 0);
    assert_eq!(state.turn_queue.current(), Some(uid(1)));

    let (derived, ctx) = apply_effect(&mut state, &Effect::Death { unit: uid(2) }, &NoContent);

    assert!(
        !derived.iter().any(|e| matches!(e, Effect::AdvanceTurn)),
        "Did not expect Effect::AdvanceTurn for non-current actor death; got: {:?}",
        derived
    );
    assert!(
        ctx.turn_skip_events.is_empty(),
        "Expected empty turn_skip_events for non-current actor death; got: {:?}",
        ctx.turn_skip_events
    );
}
