//! Unit tests for `CombatState::roll_initiative_for_all` and
//! `CombatState::reconcile_turn_order` (Waves 2–4).
//!
//! Waves 2–3: engine-only roller + reconciler.
//! Wave 4: summon initiative roll in `step()` pump + reconcile wiring in BumpRound.

#![allow(clippy::field_reassign_with_default)]

use std::collections::HashMap;

use storyforge::combat_engine::{
    action::Action,
    content::ContentView,
    dice::{DiceRng, ExpectedValue},
    event::Event,
    state::{CombatState, RoundPhase, Unit, UnitId},
    step::step,
    AbilityDef, AbilityId, AbilityRange, AoEShape, EffectDef, PoolKind, RegenRule, StatusDef,
    StatusId, TargetType, UnitTemplate,
};

use storyforge::combat_engine::enum_map::enum_map;

use crate::common::engine_unit::EngineUnitBuilder;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId {
    UnitId(n)
}

/// Build a minimal alive unit with optional dex_mod baked into CasterContext.
/// We build with the fluent builder then patch caster_context.dex_mod directly.
fn make_unit_with_dex(id: u64, dex_mod: i32) -> Unit {
    let mut u = EngineUnitBuilder::new(id).hp(10, 10).build();
    u.caster_context.dex_mod = dex_mod;
    u
}

/// Build a minimal dead unit (hp=0 / max=10).
fn make_dead_unit(id: u64) -> Unit {
    EngineUnitBuilder::new(id).hp(0, 10).build()
}

/// Build a `CombatState` with the given units (all with initiative=None
/// unless the builder already set it).
fn state_from(units: Vec<Unit>) -> CombatState {
    CombatState::new(units, 1, RoundPhase::PreRound, 0)
}

// ── roll_initiative_assigns_and_emits ────────────────────────────────────────

/// A normal (non-preset) unit gets `Some(d20+dex_mod)` and one event.
/// A preset unit gets the preset value and no event.
/// A unit that already has `Some(initiative)` is untouched.
#[test]
fn roll_initiative_assigns_and_emits() {
    let u1 = make_unit_with_dex(1, 2); // will be rolled; dex_mod=2
    let mut u2 = make_unit_with_dex(2, 0); // preset to 15
    let mut u3 = make_unit_with_dex(3, 0); // already has initiative=Some(9)
    u2.initiative = None; // ensure none before preset
    u3.initiative = Some(9);

    let mut state = state_from(vec![u1, u2, u3]);

    // Script one d20 roll for unit 1 (id=1 is lowest → first in sorted order).
    let mut rng = DiceRng::with_seed(42);
    rng.script(&[15]); // unit 1 draws 15

    let mut preset = HashMap::new();
    preset.insert(uid(2), 15_i32);

    let events = state.roll_initiative_for_all(&mut rng, &preset);

    // Unit 1: rolled, initiative = 15 + 2 = 17
    assert_eq!(state.unit(uid(1)).unwrap().initiative, Some(17));
    // Unit 2: preset, initiative = 15 (no roll consumed)
    assert_eq!(state.unit(uid(2)).unwrap().initiative, Some(15));
    // Unit 3: already set, untouched = 9
    assert_eq!(state.unit(uid(3)).unwrap().initiative, Some(9));

    // Only unit 1 emits an event (preset and already-set emit nothing).
    assert_eq!(events.len(), 1);
    match &events[0] {
        Event::InitiativeRolled {
            unit,
            roll,
            dex_mod,
            total,
        } => {
            assert_eq!(*unit, uid(1));
            assert_eq!(*roll, 15);
            assert_eq!(*dex_mod, 2);
            assert_eq!(*total, 17);
        }
        other => panic!("expected InitiativeRolled, got {:?}", other),
    }
}

// ── roll_order_is_unitid_sorted_deterministic ────────────────────────────────

/// With a fixed scripted sequence [7, 12, 3], the draws are consumed in
/// ascending UnitId order (UnitId 1 → 7, UnitId 2 → 12, UnitId 3 → 3).
///
/// This pins the replay-determinism guarantee: same seed + same roster
/// always assigns the same roll to the same unit.
#[test]
fn roll_order_is_unitid_sorted_deterministic() {
    // Insert units in reverse order to ensure sort isn't trivially insertion-order.
    let u3 = make_unit_with_dex(3, 0);
    let u1 = make_unit_with_dex(1, 0);
    let u2 = make_unit_with_dex(2, 0);
    let mut state = state_from(vec![u3, u1, u2]);

    let mut rng = DiceRng::with_seed(0);
    rng.script(&[7, 12, 3]); // draws in order: uid1=7, uid2=12, uid3=3

    let preset: HashMap<UnitId, i32> = HashMap::new();
    let events = state.roll_initiative_for_all(&mut rng, &preset);

    // Verify roll assignments match UnitId-ascending consumption.
    assert_eq!(events.len(), 3);
    let rolls: Vec<(UnitId, i32)> = events
        .iter()
        .map(|e| match e {
            Event::InitiativeRolled { unit, roll, .. } => (*unit, *roll),
            other => panic!("unexpected event {:?}", other),
        })
        .collect();

    // Events are emitted in UnitId-sorted processing order.
    assert_eq!(rolls[0], (uid(1), 7));
    assert_eq!(rolls[1], (uid(2), 12));
    assert_eq!(rolls[2], (uid(3), 3));

    // Verify state.
    assert_eq!(state.unit(uid(1)).unwrap().initiative, Some(7));
    assert_eq!(state.unit(uid(2)).unwrap().initiative, Some(12));
    assert_eq!(state.unit(uid(3)).unwrap().initiative, Some(3));
}

// ── reconcile_sorts_desc_with_unitid_tiebreak ────────────────────────────────

/// Given units with mixed initiatives including a tie and a None:
/// order is descending by initiative, ties broken by ascending UnitId,
/// None-initiative units sort last, dead units ARE included.
#[test]
fn reconcile_sorts_desc_with_unitid_tiebreak() {
    // uid 1: initiative=Some(10) (alive)
    // uid 2: initiative=Some(15) (alive)
    // uid 3: initiative=Some(10) — TIE with uid 1 (alive)
    // uid 4: initiative=None      — sorts last (alive)
    // uid 5: initiative=Some(5)  — dead, still in order
    let mut u1 = make_unit_with_dex(1, 0);
    u1.initiative = Some(10);
    let mut u2 = make_unit_with_dex(2, 0);
    u2.initiative = Some(15);
    let mut u3 = make_unit_with_dex(3, 0);
    u3.initiative = Some(10);
    let mut u4 = make_unit_with_dex(4, 0);
    u4.initiative = None;
    let mut u5 = make_dead_unit(5);
    u5.initiative = Some(5);

    let mut state = state_from(vec![u1, u2, u3, u4, u5]);
    state.reconcile_turn_order();

    let order = &state.turn_queue.order;
    assert_eq!(order.len(), 5);

    // Expected order: uid2(15) > uid1(10) = uid3(10) tie→uid1<uid3 > uid5(5) > uid4(None=MIN)
    assert_eq!(order[0], uid(2)); // 15
    assert_eq!(order[1], uid(1)); // 10, lower id wins tie
    assert_eq!(order[2], uid(3)); // 10, higher id loses tie
    assert_eq!(order[3], uid(5)); // 5, dead but included
    assert_eq!(order[4], uid(4)); // None → i32::MIN, last
}

// ── reconcile_does_not_touch_index ───────────────────────────────────────────

/// `reconcile_turn_order` must NOT modify `turn_queue.index`.
#[test]
fn reconcile_does_not_touch_index() {
    let mut u1 = make_unit_with_dex(1, 0);
    u1.initiative = Some(10);
    let mut u2 = make_unit_with_dex(2, 0);
    u2.initiative = Some(5);

    let mut state = state_from(vec![u1, u2]);
    // Pre-set a non-zero index.
    state.turn_queue.index = 7;

    state.reconcile_turn_order();

    assert_eq!(state.turn_queue.index, 7, "reconcile must not touch index");
}

// ── Wave 4 tests ──────────────────────────────────────────────────────────────

// ── Helpers for Wave 4 ────────────────────────────────────────────────────────

/// Minimal `ContentView` that serves one summon ability + one unit template.
struct SummonContent {
    ability_id: AbilityId,
    ability: AbilityDef,
    template_id: String,
    template: UnitTemplate,
    status_def: StatusDef,
}

impl SummonContent {
    fn new(ability_id: &str, template_id: &str, template: UnitTemplate) -> Self {
        let ability = AbilityDef {
            key: None,
            cost_ap: 1,
            costs: vec![],
            range: AbilityRange { min: 0, max: 0 },
            target_type: TargetType::Myself,
            aoe: AoEShape::None,
            friendly_fire: false,
            effect: EffectDef::Summon {
                template_id: template_id.into(),
                max_active: None,
            },
            statuses: vec![],
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        };
        Self {
            ability_id: AbilityId::from(ability_id),
            ability,
            template_id: template_id.into(),
            template,
            status_def: StatusDef {
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                forces_targeting: false,
                skips_turn: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                bonuses: storyforge::combat_engine::StatusBonuses {
                    armor_bonus: 0,
                    damage_taken_bonus: 0,
                    speed_bonus: 0,
                },
            },
        }
    }
}

impl ContentView for SummonContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        if id == &self.ability_id {
            Some(&self.ability)
        } else {
            None
        }
    }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
        Some(&self.status_def)
    }
    fn unit_template(&self, id: &str) -> Option<UnitTemplate> {
        if id == self.template_id {
            Some(self.template.clone())
        } else {
            None
        }
    }
}

/// Build a minimal `UnitTemplate` for a summoned unit; optionally set `dex_mod`
/// on the caster_context so we can verify the dex modifier is applied to the roll.
fn summon_template(dex_mod: i32) -> UnitTemplate {
    let mut ctx: storyforge::combat_engine::CasterContext = Default::default();
    ctx.dex_mod = dex_mod;
    UnitTemplate {
        max_hp: 8,
        armor: 0,
        base_speed: 4,
        max_ap: 1,
        mana_max: 0,
        energy_max: 0,
        rage_max: 0,
        caster_context: ctx,
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        regen_per_pool: enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        initial_statuses: Vec::new(),
        initial_pools: enum_map! {
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

// ── summon_rolls_initiative_and_acts_next_round ───────────────────────────────

/// A summoner casts a Summon mid-combat.
///
/// Assert:
///  1. `Event::InitiativeRolled` appears for the new unit in the spawn step.
///  2. `roll + dex_mod == total` in the event.
///  3. The summon's `initiative` field on the state matches `total`.
///  4. The summon is NOT yet in `turn_queue.order` during the spawn step
///     (reconcile only fires on BumpRound).
///  5. After a BumpRound (end of summoner's turn + end of round), the summon IS
///     in the order at the correct initiative-sorted position, and gets a
///     `TurnStarted` event once the round ticks over.
#[test]
fn summon_rolls_initiative_and_acts_next_round() {
    use storyforge::combat_engine::state::Team;
    use storyforge::game::hex::hex_from_offset;

    // ── Setup ────────────────────────────────────────────────────────────────
    // Summoner (uid=1, Player, initiative=10) at (0,0).
    // Enemy   (uid=2, Enemy,  initiative=5)  at (5,0) — guarantees a non-empty
    //         enemy list so the phase never jumps to Victory immediately.
    let dex_mod = 3;
    let content = SummonContent::new("summon", "imp", summon_template(dex_mod));

    let mut summoner = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos(0, 0)
        .initiative(10)
        .ap(2, 2)
        .build();
    summoner.caster_context.dex_mod = 0; // summoner's own dex doesn't matter here

    let enemy = EngineUnitBuilder::new(2)
        .team(Team::Enemy)
        .pos(5, 0)
        .initiative(5)
        .build();

    let mut state = CombatState::new(vec![summoner, enemy], 1, RoundPhase::ActorTurn, 0);
    // Pre-set turn order: summoner first (initiative=10), enemy second (5).
    state.turn_queue.order = vec![UnitId(1), UnitId(2)];
    state.turn_queue.index = 0;

    // Two d20 rolls happen during Action::Cast for a Summon:
    //   1. crit_fail check (any value != 1 is fine; we use 10)
    //   2. summoned unit's initiative roll (we script 12 → total = 12 + 3 = 15)
    let scripted_roll = 12_i32;
    let expected_total = scripted_roll + dex_mod;
    let mut rng = DiceRng::with_seed(0);
    rng.script(&[10, scripted_roll]); // [crit_fail_die, initiative_die]

    // ── Spawn step ───────────────────────────────────────────────────────────
    let (spawn_events, _) = step(
        &mut state,
        Action::Cast {
            actor: UnitId(1),
            ability: AbilityId::from("summon"),
            target: UnitId(1),
            target_pos: hex_from_offset(0, 0),
        },
        &mut rng,
        &content,
    )
    .expect("summon cast should succeed");

    // 1. InitiativeRolled event for the new unit.
    let init_ev = spawn_events
        .iter()
        .find(|e| matches!(e, Event::InitiativeRolled { .. }))
        .expect("InitiativeRolled must appear in spawn step events");

    let (ev_uid, ev_roll, ev_dex, ev_total) = match init_ev {
        Event::InitiativeRolled {
            unit,
            roll,
            dex_mod,
            total,
        } => (*unit, *roll, *dex_mod, *total),
        _ => unreachable!(),
    };

    // 2. roll + dex_mod == total.
    assert_eq!(
        ev_roll + ev_dex,
        ev_total,
        "total must equal roll + dex_mod"
    );
    assert_eq!(ev_dex, dex_mod);
    assert_eq!(ev_total, expected_total);

    // 3. State initiative field updated.
    let new_uid = ev_uid;
    assert_eq!(
        state.unit(new_uid).unwrap().initiative,
        Some(expected_total),
        "summoned unit's initiative field must equal rolled total",
    );

    // 4. Summon NOT yet in turn_queue.order (reconcile deferred to next BumpRound).
    assert!(
        !state.turn_queue.order.contains(&new_uid),
        "summon must NOT be in turn_queue.order during its spawn step",
    );

    // ── BumpRound: end summoner turn, let enemy end, then round rolls over ───
    // End summoner's turn.
    let _ = step(
        &mut state,
        Action::EndTurn { actor: UnitId(1) },
        &mut ExpectedValue,
        &content,
    )
    .expect("end summoner turn");

    // End enemy's turn — this triggers BumpRound.
    let (bump_events, _) = step(
        &mut state,
        Action::EndTurn { actor: UnitId(2) },
        &mut ExpectedValue,
        &content,
    )
    .expect("end enemy turn / BumpRound");

    // 5a. Summon IS now in the order.
    assert!(
        state.turn_queue.order.contains(&new_uid),
        "summon must be in turn_queue.order after BumpRound",
    );

    // 5b. Summon is at the correct position (initiative=15 > summoner=10 > enemy=5).
    let pos = state
        .turn_queue
        .order
        .iter()
        .position(|&id| id == new_uid)
        .expect("summon uid in order");
    let summoner_pos = state
        .turn_queue
        .order
        .iter()
        .position(|&id| id == UnitId(1))
        .expect("summoner uid in order");
    assert!(
        pos < summoner_pos,
        "summon (initiative={expected_total}) must sort before summoner (initiative=10)",
    );

    // 5c. TurnStarted emitted for someone (round 2 begins).
    assert!(
        bump_events
            .iter()
            .any(|e| matches!(e, Event::TurnStarted { .. })),
        "TurnStarted must be emitted when BumpRound settles the new round",
    );
}

// summon_replay_hash_stable: see replay.rs (uses record_then_replay harness defined there)
