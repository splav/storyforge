//! Integration tests for the Heal-over-Time (HoT) path.
//!
//! Covers: engine-level TickHeal fanout, HotHealed event emission, +8 HP over
//! 2 ticks, and bridge-level CombatLog::HotHealed entry.

use std::collections::HashMap;

use storyforge::combat_engine::content::StatusBonuses;
use storyforge::combat_engine::{
    self,
    content::{AbilityDef, ContentView, StatusDef},
    event::Event,
    state::{ActiveStatus, CombatState, EffectSource, RoundPhase, Team, UnitId},
    AbilityId, StatusId,
};

// ── ContentView stub ──────────────────────────────────────────────────────────

struct HotContent {
    statuses: HashMap<StatusId, StatusDef>,
}

impl HotContent {
    fn with_vital_infusion() -> Self {
        let mut statuses = HashMap::new();
        statuses.insert(
            StatusId::from("vital_infusion"),
            StatusDef {
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                forces_targeting: false,
                skips_turn: false,
                bonuses: StatusBonuses {
                    runtime: storyforge::combat_engine::RuntimeStatsDelta(Default::default()),
                },
                hp_percent_dot: 0,
                heal_per_tick: 4,
                ..Default::default()
            },
        );
        Self { statuses }
    }
}

impl ContentView for HotContent {
    fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> {
        None
    }
    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        self.statuses.get(id)
    }
    fn unit_template(&self, _: &str) -> Option<combat_engine::UnitTemplate> {
        None
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn make_unit(id: u64, hp: i32, max_hp: i32) -> combat_engine::state::Unit {
    crate::common::engine_unit::EngineUnitBuilder::new(id)
        .team(Team::Player)
        .hp(hp, max_hp)
        .build()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Applying a HoT status and ticking it twice yields +8 HP total.
#[test]
fn hot_two_ticks_yield_plus_eight_hp() {
    let uid = UnitId(1);
    let mut unit = make_unit(1, 12, 20); // 8 HP missing
    unit.statuses.push(ActiveStatus {
        id: StatusId::from("vital_infusion"),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: EffectSource::Unit(uid),
    });
    let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    let content = HotContent::with_vital_infusion();

    // Tick 1
    let ev1 = state.tick_actor_statuses(uid, &content);
    let hot1 = ev1.iter().find(|e| {
        matches!(e,
            Event::HotHealed { target, source_status, amount }
            if *target == uid && source_status.0 == "vital_infusion" && *amount == 4
        )
    });
    assert!(
        hot1.is_some(),
        "HotHealed(+4) expected on tick 1; got: {:?}",
        ev1
    );
    assert_eq!(
        state.unit(uid).unwrap().hp(),
        16,
        "HP should be 16 after tick 1"
    );

    // Tick 2
    let ev2 = state.tick_actor_statuses(uid, &content);
    let hot2 = ev2.iter().find(|e| {
        matches!(e,
            Event::HotHealed { target, source_status, amount }
            if *target == uid && source_status.0 == "vital_infusion" && *amount == 4
        )
    });
    assert!(
        hot2.is_some(),
        "HotHealed(+4) expected on tick 2; got: {:?}",
        ev2
    );
    assert_eq!(
        state.unit(uid).unwrap().hp(),
        20,
        "+8 HP total after 2 ticks"
    );
}

/// After 2 ticks the status is removed by ExpireStatus.
#[test]
fn hot_status_removed_after_duration() {
    let uid = UnitId(1);
    let mut unit = make_unit(1, 2, 20);
    unit.statuses.push(ActiveStatus {
        id: StatusId::from("vital_infusion"),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: EffectSource::Unit(uid),
    });
    let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    let content = HotContent::with_vital_infusion();

    state.tick_actor_statuses(uid, &content);
    state.tick_actor_statuses(uid, &content);

    assert!(
        state.unit(uid).unwrap().statuses.is_empty(),
        "vital_infusion should be removed after 2 ticks"
    );
}

/// DoT and HoT from different appliers tick independently.
/// HoT fires on healer's turn; DoT fires on poisoner's turn.
#[test]
fn dot_and_hot_from_different_appliers_tick_independently() {
    let healer = UnitId(1);
    let poisoner = UnitId(2);
    let victim = UnitId(3);

    let healer_unit = make_unit(1, 10, 10);
    let poisoner_unit = make_unit(2, 10, 10);
    let mut victim_unit = make_unit(3, 10, 20);

    // HoT from healer
    victim_unit.statuses.push(ActiveStatus {
        id: StatusId::from("vital_infusion"),
        rounds_remaining: 2,
        dot_per_tick: 0,
        applier: EffectSource::Unit(healer),
    });
    // DoT from poisoner (flat dot_per_tick, no content percent needed)
    victim_unit.statuses.push(ActiveStatus {
        id: StatusId::from("poison"),
        rounds_remaining: 2,
        dot_per_tick: 3,
        applier: EffectSource::Unit(poisoner),
    });

    let mut state = CombatState::new(
        vec![healer_unit, poisoner_unit, victim_unit],
        1,
        RoundPhase::ActorTurn,
        0,
    );
    let content = HotContent::with_vital_infusion(); // poison not in content → hp_percent_dot=0

    // Healer's turn: HoT should fire, DoT must not.
    let healer_events = state.tick_actor_statuses(healer, &content);
    assert!(
        healer_events.iter().any(|e| matches!(e,
            Event::HotHealed { target, source_status, .. }
            if *target == victim && source_status.0 == "vital_infusion"
        )),
        "HotHealed must fire on healer's turn"
    );
    assert!(
        !healer_events.iter().any(|e| matches!(e,
            Event::DotDamaged { target, source_status, .. }
            if *target == victim && source_status.0 == "poison"
        )),
        "DotDamaged must NOT fire on healer's turn"
    );
    assert_eq!(state.unit(victim).unwrap().hp(), 14, "HP +4 after HoT tick");

    // Poisoner's turn: DoT should fire, HoT must not.
    let poisoner_events = state.tick_actor_statuses(poisoner, &content);
    assert!(
        poisoner_events.iter().any(|e| matches!(e,
            Event::DotDamaged { target, source_status, .. }
            if *target == victim && source_status.0 == "poison"
        )),
        "DotDamaged must fire on poisoner's turn"
    );
    assert!(
        !poisoner_events.iter().any(|e| matches!(e,
            Event::HotHealed { target, source_status, .. }
            if *target == victim && source_status.0 == "vital_infusion"
        )),
        "HotHealed must NOT fire on poisoner's turn"
    );
    assert_eq!(state.unit(victim).unwrap().hp(), 11, "HP -3 after DoT tick");
}
