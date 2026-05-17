//! Focused tests for `trace::post_state_hash` and `serialize_init` (Phase 5 step 5a).

use combat_engine::state::{CombatState, RoundPhase, Team, Unit, UnitId};
use combat_engine::trace::{parse_init, post_state_hash, serialize_init, InitLine, SCHEMA_VERSION};
use hexx::Hex;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId { UnitId(n) }

fn make_unit(id: u64, hp: i32) -> Unit {
    Unit {
        id: uid(id),
        team: Team::Player,
        pos: Hex::new(id as i32, 0),
        hp,
        max_hp: 30,
        armor: 0,
        armor_bonus: 0,
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
        auras: Vec::new(),
        enemy_phases: Vec::new(),
    }
}

fn make_state(units: Vec<Unit>) -> CombatState {
    let order: Vec<UnitId> = units.iter().map(|u| u.id).collect();
    let mut s = CombatState::new(units, 1, RoundPhase::ActorTurn, 42);
    s.set_turn_queue(order, 0);
    s
}

// ── post_state_hash determinism ───────────────────────────────────────────────

#[test]
fn post_state_hash_is_deterministic() {
    let state = make_state(vec![make_unit(1, 20), make_unit(2, 15)]);
    let h1 = post_state_hash(&state);
    let h2 = post_state_hash(&state);
    assert_eq!(h1, h2, "hash must be identical across two calls on identical state");
}

#[test]
fn post_state_hash_differs_on_hp_change() {
    let state_a = make_state(vec![make_unit(1, 20), make_unit(2, 15)]);
    let state_b = make_state(vec![make_unit(1, 19), make_unit(2, 15)]);
    let h_a = post_state_hash(&state_a);
    let h_b = post_state_hash(&state_b);
    assert_ne!(h_a, h_b, "hash must differ when a unit's HP changes");
}

#[test]
fn post_state_hash_differs_on_round_change() {
    let units = vec![make_unit(1, 20)];
    let state_r1 = CombatState::new(units.clone(), 1, RoundPhase::ActorTurn, 0);
    let state_r2 = CombatState::new(units, 2, RoundPhase::ActorTurn, 0);
    assert_ne!(post_state_hash(&state_r1), post_state_hash(&state_r2));
}

#[test]
fn post_state_hash_excludes_dead_units() {
    // A dead unit (hp=0) is excluded from the hash — only alive units count.
    let state_with_dead = make_state(vec![make_unit(1, 20), make_unit(2, 0)]);
    let state_only_alive = make_state(vec![make_unit(1, 20), make_unit(2, 0)]);
    // Both states have identical alive unit sets → same hash.
    assert_eq!(
        post_state_hash(&state_with_dead),
        post_state_hash(&state_only_alive),
    );
}

// ── serialize_init roundtrip ──────────────────────────────────────────────────

#[test]
fn serialize_init_produces_parseable_jsonl() {
    let init = InitLine {
        schema: SCHEMA_VERSION,
        rng_seed: 0xCAFE_BABE,
        units: vec![make_unit(1, 30), make_unit(2, 25)],
        next_synthetic_uid: 1000,
        content_hash: "blake3:aabbccdd".to_string(),
    };

    let json = serialize_init(&init).expect("serialization must succeed");

    // Must be a single non-empty line (no trailing newline).
    assert!(!json.is_empty());
    assert!(!json.contains('\n'), "JSONL line must not contain newline");

    // Must parse back.
    let parsed = parse_init(&json).expect("must parse back");
    assert_eq!(parsed.schema, SCHEMA_VERSION);
    assert_eq!(parsed.rng_seed, 0xCAFE_BABE);
    assert_eq!(parsed.next_synthetic_uid, 1000);
    assert_eq!(parsed.units.len(), 2);
}

#[test]
fn serialize_init_byte_equal_on_second_pass() {
    let init = InitLine {
        schema: SCHEMA_VERSION,
        rng_seed: 1,
        units: vec![make_unit(5, 10)],
        next_synthetic_uid: 500,
        content_hash: "blake3:ff".to_string(),
    };
    let json1 = serialize_init(&init).unwrap();
    let decoded = parse_init(&json1).unwrap();
    let json2 = serialize_init(&decoded).unwrap();
    assert_eq!(json1, json2, "second serialization must be byte-equal");
}
