//! LOS parity tests.
//!
//! Verify that all three `ActionState` backends (Bevy / snapshot / engine) agree
//! on `is_blocked_los` and each delegates to the canonical `combat_engine::has_los`
//! — three fixed-case tests plus one randomised property test.

use std::collections::HashSet;

use storyforge::combat_engine::state::CombatState;
use storyforge::combat_engine::{has_los, ActionState};
use storyforge::game::hex::{hex_from_offset, Hex};

// ── Bevy backend ──────────────────────────────────────────────────────────────

/// Build a minimal `BevyActions` with only `blocked_hexes` populated.
/// Other fields are stubs — `is_blocked_los` only touches `blocked_hexes`.
fn bevy_blocked_los(from: Hex, to: Hex, blocked: &HashSet<Hex>) -> bool {
    use bevy::prelude::*;
    use storyforge::combat::legality_adapter::BevyActions;
    use storyforge::content::content_view::ActiveContent;
    use storyforge::game::components::{ValidationActorQ, ValidationTargetQ};
    use storyforge::game::resources::HexPositions;

    // We cannot construct real Bevy queries outside an ECS context, so we use
    // a minimal App to run a one-shot system that captures the result.
    use bevy::ecs::system::RunSystemOnce;

    let blocked_clone: HashSet<Hex> = blocked.clone();

    let result = std::sync::Arc::new(std::sync::Mutex::new(false));
    let result_write = result.clone();

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<HexPositions>();
    app.init_resource::<ActiveContent>();

    let from_c = from;
    let to_c = to;

    app.world_mut()
        .run_system_once(
            move |content: Res<ActiveContent>,
                  positions: Res<HexPositions>,
                  actor_q: Query<ValidationActorQ>,
                  target_q: Query<ValidationTargetQ>| {
                let adapter = BevyActions {
                    content: &content,
                    positions: &positions,
                    actors: &actor_q,
                    targets: &target_q,
                    blocked_hexes: &blocked_clone,
                };
                *result_write.lock().unwrap() = adapter.is_blocked_los(from_c, to_c);
            },
        )
        .unwrap();

    let val = *result.lock().unwrap();
    val
}

// ── Snapshot backend ──────────────────────────────────────────────────────────

fn snapshot_blocked_los(from: Hex, to: Hex, blocked: &HashSet<Hex>) -> bool {
    use storyforge::combat::ai::action_state::SnapshotActionState;
    use storyforge::combat::ai::world::cache::AiCache;
    use storyforge::combat::ai::world::snapshot::BattleSnapshot;

    let mut state = CombatState::default();
    state.blocked_hexes = blocked.clone();
    let snap = BattleSnapshot::new(state, AiCache::default());

    // Content is not needed for is_blocked_los — use empty content view.
    use storyforge::content::content_view::ActiveContentData;
    let content = ActiveContentData::default();
    let adapter = SnapshotActionState {
        content: &content,
        snap: &snap,
    };
    adapter.is_blocked_los(from, to)
}

// ── Engine backend ────────────────────────────────────────────────────────────

fn engine_blocked_los(from: Hex, to: Hex, blocked: &HashSet<Hex>) -> bool {
    use storyforge::combat_engine::toml_content_view::TomlContentView;
    use storyforge::combat_engine::EngineCheckState;

    let mut state = CombatState::default();
    state.blocked_hexes = blocked.clone();

    // Minimal content; is_blocked_los doesn't need any content lookups.
    let toml_content = TomlContentView::empty();
    let adapter = EngineCheckState {
        state: &state,
        content: &toml_content,
    };
    adapter.is_blocked_los(from, to)
}

// ── Direct has_los reference ──────────────────────────────────────────────────

fn direct_blocked_los(from: Hex, to: Hex, blocked: &HashSet<Hex>) -> bool {
    !has_los(from, to, |h| blocked.contains(&h))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// BevyActions::is_blocked_los matches has_los for a fixed set of cases.
#[test]
fn bevy_actions_is_blocked_los_matches_has_los() {
    let a = hex_from_offset(0, 0);
    let b = hex_from_offset(4, 0);

    // Get intermediate hex on the line from a to b.
    let cells: Vec<Hex> = a.line_to(b).collect();
    assert!(cells.len() >= 3, "need intermediates for this test");
    let mid = cells[cells.len() / 2];

    // No blocker — LOS clear.
    let empty: HashSet<Hex> = HashSet::new();
    assert_eq!(
        bevy_blocked_los(a, b, &empty),
        direct_blocked_los(a, b, &empty),
        "empty blocked_hexes: Bevy should agree with has_los"
    );

    // Blocker on intermediate — LOS blocked.
    let blocked: HashSet<Hex> = [mid].into_iter().collect();
    assert_eq!(
        bevy_blocked_los(a, b, &blocked),
        direct_blocked_los(a, b, &blocked),
        "blocker on mid: Bevy should agree with has_los"
    );

    // Blocker only on endpoints — LOS still clear (endpoints excluded).
    let endpoint_blocked: HashSet<Hex> = [a, b].into_iter().collect();
    assert_eq!(
        bevy_blocked_los(a, b, &endpoint_blocked),
        direct_blocked_los(a, b, &endpoint_blocked),
        "endpoint blockers only: Bevy should agree with has_los (endpoints excluded)"
    );

    // Self-LOS — always clear.
    assert!(
        !bevy_blocked_los(a, a, &blocked),
        "self-LOS should never be blocked"
    );
}

/// SnapshotActionState::is_blocked_los matches has_los for a fixed set of cases.
#[test]
fn snapshot_actions_is_blocked_los_matches_has_los() {
    let a = hex_from_offset(0, 0);
    let b = hex_from_offset(4, 0);

    let cells: Vec<Hex> = a.line_to(b).collect();
    let mid = cells[cells.len() / 2];

    let empty: HashSet<Hex> = HashSet::new();
    assert_eq!(
        snapshot_blocked_los(a, b, &empty),
        direct_blocked_los(a, b, &empty),
        "empty: Snapshot agrees with has_los"
    );

    let blocked: HashSet<Hex> = [mid].into_iter().collect();
    assert_eq!(
        snapshot_blocked_los(a, b, &blocked),
        direct_blocked_los(a, b, &blocked),
        "blocker on mid: Snapshot agrees with has_los"
    );

    let endpoint_blocked: HashSet<Hex> = [a, b].into_iter().collect();
    assert_eq!(
        snapshot_blocked_los(a, b, &endpoint_blocked),
        direct_blocked_los(a, b, &endpoint_blocked),
        "endpoint blockers only: Snapshot agrees with has_los"
    );

    assert!(
        !snapshot_blocked_los(a, a, &blocked),
        "self-LOS should never be blocked"
    );
}

/// EngineCheckState::is_blocked_los matches has_los for a fixed set of cases.
#[test]
fn engine_action_state_is_blocked_los_matches_has_los() {
    let a = hex_from_offset(0, 0);
    let b = hex_from_offset(4, 0);

    let cells: Vec<Hex> = a.line_to(b).collect();
    let mid = cells[cells.len() / 2];

    let empty: HashSet<Hex> = HashSet::new();
    assert_eq!(
        engine_blocked_los(a, b, &empty),
        direct_blocked_los(a, b, &empty),
        "empty: Engine agrees with has_los"
    );

    let blocked: HashSet<Hex> = [mid].into_iter().collect();
    assert_eq!(
        engine_blocked_los(a, b, &blocked),
        direct_blocked_los(a, b, &blocked),
        "blocker on mid: Engine agrees with has_los"
    );

    let endpoint_blocked: HashSet<Hex> = [a, b].into_iter().collect();
    assert_eq!(
        engine_blocked_los(a, b, &endpoint_blocked),
        direct_blocked_los(a, b, &endpoint_blocked),
        "endpoint blockers only: Engine agrees with has_los"
    );

    assert!(
        !engine_blocked_los(a, a, &blocked),
        "self-LOS should never be blocked"
    );
}

/// Property test: all three backends agree on is_blocked_los for random inputs.
///
/// Parity is structural — `is_blocked_los` is a shared trait default-impl over a
/// `blocked_hexes()` getter, so this guards against an accidental future override.
/// n_cases is kept at 60 because `bevy_blocked_los` spins up a `MinimalPlugins`
/// App per call; 60 keeps the test well under 10 s.
#[test]
fn prop_all_three_backends_agree_on_los() {
    // Simple LCG for reproducible pseudo-random numbers without external deps.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            let span = (hi - lo) as u64;
            lo + (self.next() % span) as i32
        }
    }

    let mut rng = Lcg(0xdeadbeef_cafebabe);
    let mut failures = 0usize;
    let n_cases = 60;

    for i in 0..n_cases {
        let from_col = rng.range(0, 8);
        let from_row = rng.range(0, 7);
        let to_col = rng.range(0, 8);
        let to_row = rng.range(0, 7);

        let from = hex_from_offset(from_col, from_row);
        let to = hex_from_offset(to_col, to_row);

        // Build a random blocked_hexes set (0..4 random hexes from line).
        let mut blocked: HashSet<Hex> = HashSet::new();
        let n_blockers = rng.range(0, 5) as usize;
        for _ in 0..n_blockers {
            let bc = rng.range(0, 8);
            let br = rng.range(0, 7);
            blocked.insert(hex_from_offset(bc, br));
        }

        let expected = direct_blocked_los(from, to, &blocked);
        let snap_r = snapshot_blocked_los(from, to, &blocked);
        let eng_r = engine_blocked_los(from, to, &blocked);
        let bevy_r = bevy_blocked_los(from, to, &blocked);

        if snap_r != expected || eng_r != expected || bevy_r != expected {
            eprintln!(
                "case {i}: from=({from_col},{from_row}) to=({to_col},{to_row}) \
                 blocked={n_blockers} expected={expected} snap={snap_r} eng={eng_r} bevy={bevy_r}"
            );
            failures += 1;
        }
    }

    assert_eq!(
        failures, 0,
        "{failures}/{n_cases} property-test cases failed (snapshot/engine/bevy vs has_los)"
    );
}
