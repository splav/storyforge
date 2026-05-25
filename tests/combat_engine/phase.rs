//! Engine integration tests for `Effect::EnterPhase` — Phase 4 step 4d.
//!
//! Covers:
//! - Phase trigger fires on Damage crossing threshold.
//! - Phase preempts death: lethal damage + phase + heal_to_full → alive, no Died.
//! - Non-triggering damage does NOT push EnterPhase.
//! - Multi-threshold AoE: each Damage effect fires its own phase check.
//! - Phase cascade applies all atomic effects: SetMaxHp, Heal, RefreshAggregates.
//! - Event::PhaseEntered carries correct prev_max_hp / new_max_hp.

use storyforge::combat_engine::{
    content::{ContentView, PhaseEntry},
    event::Event,
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    StatusDef, StatusId,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId { UnitId(n) }

fn make_unit(id: u64, hp: i32, max_hp: i32) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Unit {
        id: uid(id),
        team: Team::Enemy,
        pos: hexx::Hex::ZERO,
        hp,
        max_hp,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 3,
        speed: 3,
        reactions_left: 1,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((3, 3)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
    }
}

/// Build a boss unit with pre-set phase triggers.
fn make_boss(id: u64, hp: i32, max_hp: i32, phases: Vec<PhaseEntry>) -> Unit {
    let mut u = make_unit(id, hp, max_hp);
    u.enemy_phases = phases;
    u
}

fn make_attacker(id: u64) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Unit {
        id: uid(id),
        team: Team::Player,
        pos: hexx::Hex::new(2, 0),
        hp: 30,
        max_hp: 30,
        armor: 0,
        armor_bonus: 0,
        damage_taken_bonus: 0,
        base_speed: 3,
        speed: 3,
        reactions_left: 0,
        reactions_max: 1,
        statuses: vec![],
        summoner: None,
        caster_context: Default::default(),
        aoo_dice: None,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        pools: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((3, 3)),
            PoolKind::Mp     => Some((3, 3)),
        },
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
    }
}

fn make_state(units: Vec<Unit>, order: Vec<UnitId>) -> CombatState {
    let mut s = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);
    s.set_turn_queue(order.clone(), 0);
    s
}

/// Minimal ContentView stub for phase tests — no status/ability data needed.
struct PhaseContent;

impl PhaseContent {
    fn new(_boss_id: UnitId, _pct: i32, _new_max_hp: i32, _heal_to_full: bool) -> Self {
        // Phase data now lives on Unit.enemy_phases; this stub carries none.
        Self
    }
}

impl ContentView for PhaseContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId)
        -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

/// Minimal ContentView stub for two-phase tests.
///
/// Phase data lives on `Unit.enemy_phases`. The engine's `Effect::EnterPhase`
/// handler pops `enemy_phases[0]` automatically after consuming the transition.
struct TwoPhaseContent;

impl TwoPhaseContent {
    fn new(_boss_id: UnitId, _pct0: i32, _pct1: i32, _new_max_hp: i32) -> Self {
        // Phase data is now set on the unit via make_boss(); this stub carries none.
        Self
    }
}

impl ContentView for TwoPhaseContent {
    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId)
        -> Option<&storyforge::combat_engine::AbilityDef> { None }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { None }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

// Helper to apply raw damage to a unit via the effect system.
// We use Effect::Damage directly via apply_effect to isolate phase checks.
use storyforge::combat_engine::effect::{apply_effect, Effect};

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Phase trigger fires when hp crosses the 50% threshold.
#[test]
fn phase_trigger_fires_at_threshold() {
    let boss = uid(1);
    let attacker = uid(2);
    // Boss: 100 max_hp, starts at 60 hp. Damage=25 → hp becomes 35 → crosses 50%.
    let mut state = make_state(
        vec![
            make_boss(1, 60, 100, vec![PhaseEntry { pct: 50, new_max_hp: 120, heal_to_full: false }]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 120, false);

    // Apply a Damage effect of 25 raw (armor=0 → final=25; hp 60→35).
    let (derived, _ctx) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 25.0, source: attacker, pierces: false },
        &content,
    );

    // Should derive GainRage×2 then EnterPhase — NOT Death.
    let has_enter_phase = derived.iter().any(|e| matches!(e, Effect::EnterPhase { unit, .. } if *unit == boss));
    let has_death = derived.iter().any(|e| matches!(e, Effect::Death { unit } if *unit == boss));
    assert!(has_enter_phase, "EnterPhase should be derived when threshold crossed; got {derived:?}");
    assert!(!has_death, "Death must NOT be derived when phase fires");
    assert_eq!(state.unit(boss).unwrap().hp, 35, "hp should be 35 after 25 damage");
}

/// Non-triggering damage (hp stays above threshold) does NOT produce EnterPhase.
#[test]
fn non_triggering_damage_no_enter_phase() {
    let boss = uid(1);
    let attacker = uid(2);
    // Boss at 90 hp (100 max). Damage=10 → hp=80 → still above 50%.
    let mut state = make_state(
        vec![
            make_boss(1, 90, 100, vec![PhaseEntry { pct: 50, new_max_hp: 120, heal_to_full: false }]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 120, false);

    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 10.0, source: attacker, pierces: false },
        &content,
    );

    let has_enter_phase = derived.iter().any(|e| matches!(e, Effect::EnterPhase { .. }));
    let has_death = derived.iter().any(|e| matches!(e, Effect::Death { .. }));
    assert!(!has_enter_phase, "EnterPhase must not fire when hp stays above threshold");
    assert!(!has_death, "Death must not fire either (hp=80 > 0)");
}

/// Lethal damage with a phase trigger fires EnterPhase and heal_to_full=true
/// restores the unit; the unit ends up alive with no Died event in the stream.
#[test]
fn preempt_death_phase_revives_unit() {
    let boss = uid(1);
    let attacker = uid(2);
    // Boss at 60 hp (100 max). Damage=70 → hp would go to -10 → lethal.
    // Phase fires at 50% with heal_to_full=true and new_max_hp=120.
    let mut state = make_state(
        vec![
            make_boss(1, 60, 100, vec![PhaseEntry { pct: 50, new_max_hp: 120, heal_to_full: true }]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 120, true);

    // Phase triggers here (hp goes to 0 clamped by `u.hp = (u.hp - dmg).max(0)`):
    // actual engine code: hp = (60 - 70).max(0) = 0; 0 * 100 <= 100 * 50 → trigger.
    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 70.0, source: attacker, pierces: false },
        &content,
    );

    // Must derive EnterPhase, NOT Death.
    let has_enter_phase = derived.iter().any(|e| matches!(e, Effect::EnterPhase { unit, .. } if *unit == boss));
    let has_death = derived.iter().any(|e| matches!(e, Effect::Death { unit } if *unit == boss));
    assert!(has_enter_phase, "EnterPhase should fire on lethal damage if threshold crossed");
    assert!(!has_death, "Death must NOT fire — phase preempts it");

    // Now apply EnterPhase and its cascade to confirm revival.
    // Apply each derived effect in order (GainRage×2, EnterPhase).
    for eff in &derived {
        let (sub_derived, _) = apply_effect(&mut state, eff, &content);
        // Apply the EnterPhase cascade (SetMaxHp, Heal, RefreshAggregates).
        for sub_eff in &sub_derived {
            apply_effect(&mut state, sub_eff, &content);
        }
    }

    let boss_unit = state.unit(boss).unwrap();
    assert!(boss_unit.is_alive(), "boss must be alive after phase revival");
    assert_eq!(boss_unit.max_hp, 120, "max_hp should be updated to 120");
    assert_eq!(boss_unit.hp, 120, "hp should equal new max_hp after heal_to_full");
}

/// Phase cascade sets max_hp, and heal_to_full restores hp to new max.
/// Also verifies Event::PhaseEntered carries correct prev_max_hp / new_max_hp.
#[test]
fn phase_cascade_sets_max_hp_and_emits_phase_entered_event() {
    use storyforge::combat_engine::event::effect_to_event;

    let boss = uid(1);
    let attacker = uid(2);
    // Boss at 60 hp (100 max). Phase at 50%; new_max_hp=150, heal_to_full=false.
    let mut state = make_state(
        vec![
            make_boss(1, 60, 100, vec![PhaseEntry { pct: 50, new_max_hp: 150, heal_to_full: false }]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 150, false);

    // Trigger with 20 raw damage (hp 60→40, crosses 50%).
    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 20.0, source: attacker, pierces: false },
        &content,
    );

    let enter_phase_effect = derived.iter().find(|e| matches!(e, Effect::EnterPhase { .. }))
        .expect("EnterPhase must be derived");

    // Apply EnterPhase and capture its ctx and sub-effects.
    let (cascade, ctx) = apply_effect(&mut state, enter_phase_effect, &content);

    // The ctx must carry prev_max_hp=100, new_max_hp=150.
    let (prev_max_hp, new_max_hp) = ctx.phase_entered.expect("phase_entered ctx must be set");
    assert_eq!(prev_max_hp, 100, "prev_max_hp should be original max_hp");
    assert_eq!(new_max_hp, 150, "new_max_hp should match PhaseTransition.new_max_hp");

    // Apply cascade: SetMaxHp, RefreshAggregates (no Heal since heal_to_full=false).
    for sub in &cascade {
        apply_effect(&mut state, sub, &content);
    }
    assert_eq!(state.unit(boss).unwrap().max_hp, 150, "max_hp should be 150 after cascade");
    // hp stays at 40 (no heal_to_full).
    assert_eq!(state.unit(boss).unwrap().hp, 40, "hp stays at 40 (no heal_to_full)");

    // effect_to_event should produce PhaseEntered.
    let event = effect_to_event(enter_phase_effect, &state, None, &ctx);
    match event {
        Some(Event::PhaseEntered { unit, phase_idx, prev_max_hp: pmh, new_max_hp: nmh }) => {
            assert_eq!(unit, boss);
            assert_eq!(phase_idx, 0);
            assert_eq!(pmh, 100);
            assert_eq!(nmh, 150);
        }
        other => panic!("expected PhaseEntered event, got {other:?}"),
    }
}

/// Multi-threshold AoE: two Damage effects both crossing different thresholds
/// each fire their own EnterPhase (not capped to one per step).
///
/// Uses direct apply_effect calls to simulate an AoE effect queue.
#[test]
fn multi_threshold_each_damage_fires_own_phase() {
    let boss = uid(1);
    let attacker = uid(2);
    // Boss: 100 max_hp, starts at 100 hp.
    // First Damage: 55 → hp=45 → crosses 50% → EnterPhase(0).
    // After phase 0 heals to full (120 new_max_hp), then…
    // Second Damage: 35 → hp=85 → does NOT cross 50% of 120 (60); 85 > 60.
    // So multi-trigger in a single step requires NOT heal_to_full or lower thresholds.
    //
    // Alternative: no heal, boss stays at 45 hp after phase 0.
    // Second Damage: 20 → hp=25 → crosses 25% of 100 = 25 → EnterPhase(1).
    // We use TwoPhaseContent for this.
    let mut state = make_state(
        vec![
            make_boss(1, 100, 100, vec![
                PhaseEntry { pct: 50, new_max_hp: 120, heal_to_full: false },
                PhaseEntry { pct: 25, new_max_hp: 120, heal_to_full: false },
            ]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = TwoPhaseContent::new(boss, 50, 25, 120);

    // First Damage (55 raw, no armor → final=55, hp 100→45, crosses 50%).
    let (derived1, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 55.0, source: attacker, pierces: false },
        &content,
    );
    let has_phase0 = derived1.iter().any(|e| matches!(e, Effect::EnterPhase { unit, phase_idx } if *unit == boss && *phase_idx == 0));
    let has_death1 = derived1.iter().any(|e| matches!(e, Effect::Death { .. }));
    assert!(has_phase0, "Phase 0 should fire after first Damage");
    assert!(!has_death1, "No Death for phase 0 trigger");
    assert_eq!(state.unit(boss).unwrap().hp, 45);

    // Apply Phase 0 cascade (SetMaxHp only; heal_to_full=false).
    // EnterPhase apply consumes `enemy_phases[0]` automatically — no manual pop.
    for eff in &derived1 {
        if matches!(eff, Effect::EnterPhase { .. }) {
            let (cascade, _) = apply_effect(&mut state, eff, &content);
            for sub in &cascade {
                apply_effect(&mut state, sub, &content);
            }
        }
    }
    // After Phase 0 cascade: max_hp=120, hp=45 (no heal).
    assert_eq!(state.unit(boss).unwrap().max_hp, 120);
    assert_eq!(state.unit(boss).unwrap().hp, 45);

    // Second Damage (20 raw → hp 45→25; 25*100=2500, 120*25=3000, 2500 <= 3000 → phase 1 fires).
    // But TwoPhaseContent phase 1 checks `max_hp` passed to check_phase_trigger.
    // The new max_hp is 120 now; threshold is 25% of 120 = 30; hp=25 <= 30 → fires.
    let (derived2, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 20.0, source: attacker, pierces: false },
        &content,
    );
    // After enemy_phases.remove(0), the next pending phase is at local index 0.
    // Engine peeks `enemy_phases[0]` and always emits phase_idx=0; the bridge
    // resolves to the absolute phase via `EnemyPhases.pending[0]` (also index 0).
    let has_phase1 = derived2.iter().any(|e| matches!(e, Effect::EnterPhase { unit, phase_idx } if *unit == boss && *phase_idx == 0));
    let has_death2 = derived2.iter().any(|e| matches!(e, Effect::Death { .. }));
    assert!(has_phase1, "Phase 1 (now at enemy_phases[0]) should fire after second Damage crosses 25% of 120");
    assert!(!has_death2, "No Death for phase 1 trigger");
}

/// Verify that a non-boss unit does not trigger phase checks.
#[test]
fn phase_trigger_does_not_fire_for_unrelated_unit() {
    let boss = uid(1);
    let attacker = uid(2);
    let other = uid(3);
    // Only the boss (uid=1) has enemy_phases; "other" has none → dies normally.
    let mut state = make_state(
        vec![
            make_boss(1, 100, 100, vec![PhaseEntry { pct: 50, new_max_hp: 120, heal_to_full: true }]),
            make_attacker(2),
            make_unit(3, 10, 10), // no enemy_phases → dies normally
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 120, true);

    // Damage "other" to 0 (lethal).
    let (derived, _) = apply_effect(
        &mut state,
        &Effect::Damage { target: other, raw: 15.0, source: attacker, pierces: false },
        &content,
    );

    let has_enter_phase = derived.iter().any(|e| matches!(e, Effect::EnterPhase { .. }));
    let has_death = derived.iter().any(|e| matches!(e, Effect::Death { unit } if *unit == other));
    assert!(!has_enter_phase, "EnterPhase must not fire for non-boss unit");
    assert!(has_death, "Death must fire for the non-boss unit at 0 hp");
}

/// Full step()-level test: Cast that kills boss triggers phase, unit ends alive.
/// Uses Action::EndTurn to keep the test self-contained without Cast plumbing.
///
/// Instead, directly verify via apply_effect (unit-level) that the preempt-death
/// chain produces no Died event in the full derived set.
#[test]
fn preempt_death_no_died_event_in_stream() {
    use storyforge::combat_engine::event::effect_to_event;

    let boss = uid(1);
    let attacker = uid(2);
    // Boss: 100 max_hp, 60 hp. Lethal damage (100 raw, no armor).
    // Phase at 50%, heal_to_full=true, new_max_hp=100 (same as original).
    let mut state = make_state(
        vec![
            make_boss(1, 60, 100, vec![PhaseEntry { pct: 50, new_max_hp: 100, heal_to_full: true }]),
            make_attacker(2),
        ],
        vec![attacker, boss],
    );
    let content = PhaseContent::new(boss, 50, 100, true);

    let (derived, ctx_damage) = apply_effect(
        &mut state,
        &Effect::Damage { target: boss, raw: 100.0, source: attacker, pierces: false },
        &content,
    );

    // Simulate the effect pump: collect all events including cascade.
    let mut events: Vec<Event> = vec![];
    if let Some(ev) = effect_to_event(
        &Effect::Damage { target: boss, raw: 100.0, source: attacker, pierces: false },
        &state,
        None,
        &ctx_damage,
    ) {
        events.push(ev);
    }

    for eff in &derived {
        let (cascade, ctx2) = apply_effect(&mut state, eff, &content);
        if let Some(ev) = effect_to_event(eff, &state, None, &ctx2) {
            events.push(ev);
        }
        for sub in &cascade {
            let (_, ctx3) = apply_effect(&mut state, sub, &content);
            if let Some(ev) = effect_to_event(sub, &state, None, &ctx3) {
                events.push(ev);
            }
        }
    }

    // Must have PhaseEntered in stream.
    let has_phase_entered = events.iter().any(|e| matches!(e, Event::PhaseEntered { unit, .. } if *unit == boss));
    // Must NOT have UnitDied.
    let has_unit_died = events.iter().any(|e| matches!(e, Event::UnitDied { unit } if *unit == boss));

    assert!(has_phase_entered, "PhaseEntered must be in event stream");
    assert!(!has_unit_died, "UnitDied must NOT appear — phase preempts death");
    assert!(state.unit(boss).unwrap().is_alive(), "boss must be alive after revival");
}
