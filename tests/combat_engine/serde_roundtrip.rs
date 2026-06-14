//! Serde round-trip tests for all engine-public serializable types (Phase 5 gate §7 item 1).
//!
//! Each test asserts:
//! 1. `value` serializes to JSON without error.
//! 2. The JSON deserializes back to a value `== value`.
//! 3. A second serialization of the decoded value is **byte-equal** to the first
//!    (stable field order — §8 gotcha "Vec<Unit> final-state equality").

use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;
use storyforge::combat_engine::action::Action;
use storyforge::combat_engine::effect::{ApplyCtx, DamageCtx, Effect};
use storyforge::combat_engine::event::{Event, TurnSkipReason};
use storyforge::combat_engine::state::{
    ActiveStatus, EffectSource, EnvId, RoundPhase, Team, Unit, UnitId,
};
use storyforge::combat_engine::trace::{InitLine, StepLine, SCHEMA_VERSION};
use storyforge::combat_engine::{
    AbilityId, AuraDef, AuraEffects, CritFailOutcome, DiceExpr, PhaseTransition,
    SpawnBlockedReason, StatusBonuses, StatusId, TeamRelation, TurnQueue,
};

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T) {
    let json = serde_json::to_string(&value).unwrap();
    let decoded: T = serde_json::from_str(&json).unwrap();
    assert_eq!(value, decoded, "decoded value must equal original");
    let json2 = serde_json::to_string(&decoded).unwrap();
    assert_eq!(json, json2, "second serialization must be byte-equal");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId {
    UnitId(n)
}
fn sid(s: &str) -> StatusId {
    StatusId(s.to_string())
}
fn abid(s: &str) -> AbilityId {
    AbilityId(s.to_string())
}

use hexx::Hex;

fn unit_all_some(id: u64) -> Unit {
    crate::common::engine_unit::EngineUnitBuilder::new(id)
        .pos_hex(Hex::new(1, -1))
        .armor(3)
        .armor_bonus(1)
        .damage_taken_bonus(2)
        .base_speed_raw(4)
        .speed_only(5)
        .reactions(1, 1)
        .hp(25, 40)
        .mana(15, 20)
        .rage(7, 10)
        .energy(0, 5)
        .ap(2, 2)
        .mp(4, 4)
        .status(ActiveStatus {
            id: sid("poison"),
            rounds_remaining: 3,
            dot_per_tick: 5,
            applier: EffectSource::Unit(uid(99)),
        })
        .summoner(42)
        .build()
}

// ── Action variants ───────────────────────────────────────────────────────────

#[test]
fn action_move() {
    roundtrip(Action::Move {
        actor: uid(1),
        path: vec![Hex::new(0, 0), Hex::new(1, 0), Hex::new(2, -1)],
    });
}

#[test]
fn action_cast() {
    roundtrip(Action::Cast {
        actor: uid(2),
        ability: abid("fireball"),
        target: uid(3),
        target_pos: Hex::new(3, -2),
    });
}

#[test]
fn action_end_turn() {
    roundtrip(Action::EndTurn { actor: uid(5) });
}

// ── Event variants ────────────────────────────────────────────────────────────

#[test]
fn event_action_started() {
    roundtrip(Event::ActionStarted {
        action: Action::EndTurn { actor: uid(1) },
    });
}

#[test]
fn event_unit_moved() {
    roundtrip(Event::UnitMoved {
        actor: uid(1),
        from: Hex::new(0, 0),
        to: Hex::new(1, 0),
    });
}

#[test]
fn event_unit_damaged() {
    roundtrip(Event::UnitDamaged {
        target: uid(2),
        source: EffectSource::Unit(uid(1)),
        raw: 12.5,
        mitigation: 3,
        pierces: false,
        amount: 10,
    });
}

#[test]
fn event_unit_healed() {
    roundtrip(Event::UnitHealed {
        target: uid(3),
        amount: 8,
    });
}

#[test]
fn event_status_applied() {
    roundtrip(Event::StatusApplied {
        target: uid(1),
        status: sid("stun"),
    });
}

#[test]
fn event_status_removed() {
    roundtrip(Event::StatusRemoved {
        target: uid(1),
        status: sid("stun"),
    });
}

#[test]
fn event_turn_skipped_dead() {
    roundtrip(Event::TurnSkipped {
        actor: uid(4),
        reason: TurnSkipReason::Dead,
    });
}

#[test]
fn event_turn_skipped_stunned() {
    roundtrip(Event::TurnSkipped {
        actor: uid(4),
        reason: TurnSkipReason::Stunned,
    });
}

#[test]
fn event_round_started() {
    roundtrip(Event::RoundStarted { round: 3 });
}

#[test]
fn event_aura_status_gained() {
    roundtrip(Event::AuraStatusGained {
        target: uid(2),
        source: uid(1),
        status_id: sid("slow"),
    });
}

#[test]
fn event_aura_status_lost() {
    roundtrip(Event::AuraStatusLost {
        target: uid(2),
        source: uid(1),
        status_id: sid("slow"),
    });
}

#[test]
fn event_unit_spawned() {
    roundtrip(Event::UnitSpawned {
        uid: uid(99),
        summoner: uid(1),
        pos: Hex::new(2, 0),
        template_id: "skeleton".to_string(),
        team: Team::Enemy,
    });
}

#[test]
fn event_spawn_blocked() {
    roundtrip(Event::SpawnBlocked {
        summoner: uid(1),
        template_id: "skeleton".to_string(),
        reason: SpawnBlockedReason::MaxActiveReached,
    });
}

#[test]
fn event_phase_entered() {
    roundtrip(Event::PhaseEntered {
        unit: uid(10),
        phase_idx: 1,
        prev_max_hp: 100,
        new_max_hp: 200,
    });
}

#[test]
fn event_crit_failed_miss() {
    roundtrip(Event::CritFailed {
        actor: uid(2),
        outcome: CritFailOutcome::Miss,
    });
}

#[test]
fn event_crit_failed_self_damage() {
    roundtrip(Event::CritFailed {
        actor: uid(2),
        outcome: CritFailOutcome::SelfDamage(DiceExpr {
            count: 1,
            sides: 6,
            bonus: 0,
        }),
    });
}

#[test]
fn event_crit_failed_apply_status() {
    roundtrip(Event::CritFailed {
        actor: uid(2),
        outcome: CritFailOutcome::ApplyStatus(sid("curse")),
    });
}

// ── Effect variants ───────────────────────────────────────────────────────────

#[test]
fn effect_damage() {
    roundtrip(Effect::Damage {
        target: uid(2),
        raw: 15.0,
        source: EffectSource::Unit(uid(1)),
        pierces: false,
        magic: false,
    });
}

#[test]
fn effect_damage_env_source() {
    roundtrip(Effect::Damage {
        target: uid(2),
        raw: 5.0,
        source: EffectSource::Env(EnvId(0)),
        pierces: true,
        magic: false,
    });
}

#[test]
fn effect_heal() {
    roundtrip(Effect::Heal {
        target: uid(3),
        amount: 10,
    });
}

#[test]
fn effect_apply_status() {
    roundtrip(Effect::ApplyStatus {
        target: uid(2),
        status: sid("poison"),
        rounds: 3,
        dot_per_tick: 5,
        applier: EffectSource::Unit(uid(1)),
    });
}

#[test]
fn effect_apply_status_env_applier() {
    roundtrip(Effect::ApplyStatus {
        target: uid(2),
        status: sid("burning"),
        rounds: 2,
        dot_per_tick: 3,
        applier: EffectSource::Env(EnvId(0)),
    });
}

#[test]
fn effect_spawn() {
    roundtrip(Effect::Spawn {
        summoner: uid(1),
        template_id: "goblin".to_string(),
        max_active: Some(3),
    });
}

#[test]
fn effect_enter_phase() {
    roundtrip(Effect::EnterPhase {
        unit: uid(10),
        phase_idx: 2,
    });
}

#[test]
fn effect_move_position() {
    roundtrip(Effect::MovePosition {
        actor: uid(1),
        to: Hex::new(3, -1),
    });
}

#[test]
fn effect_advance_turn() {
    roundtrip(Effect::AdvanceTurn);
}

#[test]
fn effect_bump_round() {
    roundtrip(Effect::BumpRound);
}

// ── Unit with all-Some optional fields ───────────────────────────────────────

#[test]
fn unit_all_some_fields() {
    roundtrip(unit_all_some(7));
}

#[test]
fn unit_all_none_fields() {
    roundtrip(
        crate::common::engine_unit::EngineUnitBuilder::new(1)
            .team(Team::Enemy)
            .pos_hex(Hex::ORIGIN)
            .hp_full(10)
            .speed(3)
            .mp(3, 3)
            .reactions(0, 1)
            .build(),
    );
}

// ── InitLine / StepLine ───────────────────────────────────────────────────────

#[test]
fn init_line_roundtrip() {
    let line = InitLine {
        schema: SCHEMA_VERSION,
        session_id: "test_session".to_owned(),
        rng_seed: 0xDEAD_BEEF_1234_5678,
        units: vec![unit_all_some(1), unit_all_some(2)],
        next_synthetic_uid: 1000,
        round: 1,
        phase: RoundPhase::ActorTurn,
        turn_queue: TurnQueue {
            order: vec![uid(1), uid(2)],
            index: 0,
        },
        content_hash: "blake3:aabbcc".to_string(),
    };
    roundtrip(line);
}

#[test]
fn step_line_roundtrip() {
    let line = StepLine {
        schema: SCHEMA_VERSION,
        step: 5,
        action: Action::Cast {
            actor: uid(1),
            ability: abid("fireball"),
            target: uid(2),
            target_pos: Hex::new(3, -1),
        },
        events: vec![
            Event::UnitDamaged {
                target: uid(2),
                source: EffectSource::Unit(uid(1)),
                raw: 20.0,
                mitigation: 4,
                pierces: false,
                amount: 16,
            },
            Event::UnitDied { unit: uid(2) },
        ],
        rng_calls: 0, // populated in 5b
        post_state_hash: "blake3:deadbeef".to_string(),
    };
    roundtrip(line);
}

// ── Content types ─────────────────────────────────────────────────────────────

#[test]
fn phase_transition_roundtrip() {
    roundtrip(PhaseTransition {
        new_max_hp: 200,
        runtime: Some(storyforge::combat_engine::RuntimeStats {
            armor: 5,
            magic_resist: 2,
            base_speed: 4,
        }),
        heal_to_full: true,
        tags: None,
    });
}

#[test]
fn aura_def_roundtrip() {
    roundtrip(AuraDef {
        radius: 2,
        status_id: sid("slow"),
        applies_to: TeamRelation::Enemies,
        affects_tags: std::collections::BTreeSet::new(),
    });
}

#[test]
fn aura_def_allies() {
    roundtrip(AuraDef {
        radius: 1,
        status_id: sid("haste"),
        applies_to: TeamRelation::Allies,
        affects_tags: std::collections::BTreeSet::new(),
    });
}

#[test]
fn turn_queue_roundtrip() {
    roundtrip(TurnQueue {
        order: vec![uid(1), uid(2), uid(3)],
        index: 1,
    });
}

#[test]
fn spawn_blocked_reasons() {
    roundtrip(SpawnBlockedReason::TemplateMissing);
    roundtrip(SpawnBlockedReason::MaxActiveReached);
    roundtrip(SpawnBlockedReason::NoFreePosition);
}

#[test]
fn round_phase_variants() {
    roundtrip(RoundPhase::PreRound);
    roundtrip(RoundPhase::ActorTurn);
    roundtrip(RoundPhase::EndRound);
}

#[test]
fn crit_fail_outcome_double_cost() {
    roundtrip(CritFailOutcome::DoubleCost);
}

#[test]
fn status_bonuses_roundtrip() {
    roundtrip(StatusBonuses {
        speed_bonus: 2,
        armor_bonus: -1,
        damage_taken_bonus: 0,
    });
}

#[test]
fn aura_effects_roundtrip() {
    roundtrip(AuraEffects {
        speed_bonus: 1,
        armor_bonus: -2,
        damage_taken_bonus: 3,
        skips_turn: true,
        causes_disadvantage: false,
    });
}

#[test]
fn apply_ctx_default_roundtrip() {
    roundtrip(ApplyCtx::default());
}

#[test]
fn damage_ctx_roundtrip() {
    roundtrip(DamageCtx {
        raw: 10.5,
        mitigation: 3,
        pierces: true,
        final_amount: 10,
    });
}

#[test]
fn state_empty_blocked_hexes_roundtrip() {
    // Verify that omitting blocked_hexes (default empty) still deserialises
    // correctly even when the field is absent from old JSON.
    use storyforge::combat_engine::state::CombatState;

    let state = CombatState::new(vec![], 1, RoundPhase::ActorTurn, 0);
    let json = serde_json::to_string(&state).unwrap();
    let decoded: CombatState = serde_json::from_str(&json).unwrap();
    assert!(decoded.blocked_hexes.is_empty());
}
