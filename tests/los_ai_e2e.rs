//! E2E integration tests for T1.2.6: `requires_los = true` blocks AI/player target selection.
//!
//! All three tests work through the production `check_legality` path via
//! `SnapshotActionState`, which is the same path `generate_plans` calls when
//! deciding whether a cast is legal. This is the agreed-upon approach from
//! the T1.2.6 spec: "допускается использовать `check_legality` напрямую".
//!
//! Tests:
//! - `ai_archer_skips_target_behind_obstacle`      — ranged_shot + blocked LOS → NoLineOfSight
//! - `ai_archer_picks_alternative_target_without_los_constraint` — clear-LOS target → Ok
//! - `player_target_selection_excludes_obstructed_enemies`       — player cast → Err(NoLineOfSight)

use std::collections::HashSet;

use storyforge::combat::ai::action_state::SnapshotActionState;
use storyforge::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
use storyforge::combat_engine::legality::{check_legality, IllegalReason, ProposedAction};
use storyforge::combat_engine::AbilityId;
use storyforge::content::content_view::ContentView;
use storyforge::game::components::Team;
use storyforge::game::hex::hex_from_offset;

/// Load the global + campaign content (ranged_shot is in global, bandit_archer in campaign).
fn load_content() -> ContentView {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let campaign_dir = manifest.join("assets/data/campaigns/bell_under_veil");
    let scenario_dir = campaign_dir.join("ch2/scenarios/ch2_portside");
    ContentView::load_layered(&campaign_dir, &scenario_dir)
}

/// An obstacle wall at col=5 blocking LOS on the east side of archer at (9,4).
///
/// Positions:
///   actor (archer) at hex offset (0,0)
///   target_behind at offset (4,0) — behind obstacle at offset (2,0)
///   target_clear  at offset (4,2) — diagonal path not blocked
///   obstacle at offset (2,0)
fn obstacle_set(col: i32, row: i32) -> HashSet<hexx::Hex> {
    let mut s = HashSet::new();
    s.insert(hex_from_offset(col, row));
    s
}

// ── Test 1: ranged_shot (requires_los=true) blocked by obstacle → NoLineOfSight ──

/// AI archer cannot cast ranged_shot at a target whose LOS is blocked by an obstacle.
///
/// Setup: archer at (0,0), obstacle at (2,0), target at (4,0).
/// The obstacle sits exactly on the hex-line from archer to target.
#[test]
fn ai_archer_skips_target_behind_obstacle() {
    let content = load_content();

    let archer_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(4, 0);

    use storyforge::combat_engine::DiceExpr;
    use storyforge::content::abilities::CasterContext;
    let archer = UnitBuilder::new(1, Team::Enemy, archer_pos)
        .ap(2)
        .ability_names(&["ranged_shot"])
        .caster_ctx(CasterContext {
            ranged_dice: Some(DiceExpr::new(1, 8, 0)),
            ..Default::default()
        })
        .build();
    let target = UnitBuilder::new(2, Team::Player, target_pos).build();

    let mut snap = snapshot_from(vec![archer.clone(), target.clone()], 1);
    // Place obstacle on the direct line between archer and target.
    snap.state.blocked_hexes = obstacle_set(2, 0);

    let state = SnapshotActionState {
        content: &content,
        snap: &snap,
    };
    let bow_id = AbilityId::from("ranged_shot");

    let result = check_legality(
        ProposedAction {
            actor: archer.entity,
            ability: &bow_id,
            target: target.entity,
            target_pos,
        },
        &state,
    );

    assert_eq!(
        result,
        Err(IllegalReason::NoLineOfSight),
        "ranged_shot with blocked LOS must return NoLineOfSight"
    );
}

// ── Test 2: ranged_shot with clear LOS → Ok ──────────────────────────────────

/// AI archer CAN cast ranged_shot at a target with clear LOS (no obstacle in path).
///
/// Setup: archer at (0,0), obstacle at (2,0) (side wall), target at (4,2).
/// The LOS line from (0,0) to (4,2) does not pass through (2,0).
#[test]
fn ai_archer_picks_alternative_target_without_los_constraint() {
    let content = load_content();

    let archer_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(4, 2); // diagonal — LOS not blocked by obstacle at (2,0)

    use storyforge::combat_engine::DiceExpr;
    use storyforge::content::abilities::CasterContext;
    let archer = UnitBuilder::new(1, Team::Enemy, archer_pos)
        .ap(2)
        .ability_names(&["ranged_shot"])
        .caster_ctx(CasterContext {
            ranged_dice: Some(DiceExpr::new(1, 8, 0)),
            ..Default::default()
        })
        .build();
    let target = UnitBuilder::new(2, Team::Player, target_pos).build();

    let mut snap = snapshot_from(vec![archer.clone(), target.clone()], 1);
    // Obstacle at (2,0) is NOT on the line to target at (4,2).
    snap.state.blocked_hexes = obstacle_set(2, 0);

    let state = SnapshotActionState {
        content: &content,
        snap: &snap,
    };
    let bow_id = AbilityId::from("ranged_shot");

    let result = check_legality(
        ProposedAction {
            actor: archer.entity,
            ability: &bow_id,
            target: target.entity,
            target_pos,
        },
        &state,
    );

    assert!(
        result.is_ok(),
        "ranged_shot with clear LOS must be legal, got: {result:?}"
    );
}

// ── Test 3: player-side target selection excludes obstructed enemies ──────────

/// The player-side legality check also rejects obstructed targets for requires_los abilities.
///
/// Uses the same `check_legality` + `SnapshotActionState` path, but with the
/// actor on Team::Player — demonstrating the check is symmetric.
#[test]
fn player_target_selection_excludes_obstructed_enemies() {
    let content = load_content();

    let player_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(4, 0);

    use storyforge::combat_engine::DiceExpr;
    use storyforge::content::abilities::CasterContext;
    let player = UnitBuilder::new(1, Team::Player, player_pos)
        .ap(2)
        .ability_names(&["ranged_shot"])
        .caster_ctx(CasterContext {
            ranged_dice: Some(DiceExpr::new(1, 8, 0)),
            ..Default::default()
        })
        .build();
    let enemy = UnitBuilder::new(2, Team::Enemy, enemy_pos).build();

    let mut snap = snapshot_from(vec![player.clone(), enemy.clone()], 1);
    // Same obstacle blocking straight-line LOS.
    snap.state.blocked_hexes = obstacle_set(2, 0);

    let state = SnapshotActionState {
        content: &content,
        snap: &snap,
    };
    let bow_id = AbilityId::from("ranged_shot");

    let result = check_legality(
        ProposedAction {
            actor: player.entity,
            ability: &bow_id,
            target: enemy.entity,
            target_pos: enemy_pos,
        },
        &state,
    );

    assert_eq!(
        result,
        Err(IllegalReason::NoLineOfSight),
        "player ranged_shot at obstructed enemy must be rejected with NoLineOfSight"
    );
}

// ── Bonus: verify ch2_portside fixture parses cleanly ────────────────────────

/// The ch2_portside encounters.toml fixture must parse without panic.
#[test]
fn ch2_portside_fixture_parses() {
    use storyforge::content::encounters::load_encounters_from_str;

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let campaign_dir = manifest.join("assets/data/campaigns/bell_under_veil");
    let scenario_dir = campaign_dir.join("ch2/scenarios/ch2_portside");
    let enc_path = scenario_dir.join("encounters.toml");

    let content = ContentView::load_layered(&campaign_dir, &scenario_dir);
    let src = std::fs::read_to_string(&enc_path)
        .unwrap_or_else(|e| panic!("cannot read ch2_portside/encounters.toml: {e}"));

    let encounters = load_encounters_from_str(
        "ch2_portside",
        enc_path.to_str().unwrap(),
        &src,
        &content.unit_templates,
    );

    assert_eq!(
        encounters.len(),
        1,
        "expected exactly 1 encounter in fixture"
    );
    let enc = &encounters[0];
    assert_eq!(
        enc.obstacles.len(),
        3,
        "expected 3 obstacles (the crate wall)"
    );
    assert_eq!(enc.enemies.len(), 2, "expected 2 enemies (archer + thug)");
}
