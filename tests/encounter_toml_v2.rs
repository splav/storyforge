//! Tests for T1.1.4 and T1.2.5: TOML parsing for `keep_alive`, `all_of`,
//! `[[encounters.npcs]]`, and `[[encounters.obstacles]]`.

use std::collections::HashMap;
use storyforge::content::encounters::{load_encounters_from_str, VictoryCondition};
use storyforge::game::hex::hex_from_offset;

fn no_templates() -> HashMap<String, storyforge::content::unit_templates::UnitTemplateDef> {
    HashMap::new()
}

// ── Victory condition parsing ─────────────────────────────────────────────────

#[test]
fn parses_all_of_combined_with_keep_alive() {
    let toml = r#"
[[encounters]]
id = "test"
name = "Test"
enemies = []
victory = { type = "all_of", conditions = [
    { type = "all_enemies_dead" },
    { type = "keep_alive", target_name = "Magister", marker_color = [0.3, 0.6, 1.0] },
] }
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    assert_eq!(encounters.len(), 1);
    let enc = &encounters[0];
    if let VictoryCondition::AllOf(conditions) = &enc.victory {
        assert_eq!(conditions.len(), 2);
        assert!(matches!(conditions[0], VictoryCondition::AllEnemiesDead));
        if let VictoryCondition::KeepAlive { target_name, .. } = &conditions[1] {
            assert_eq!(target_name, "Magister");
        } else {
            panic!("expected KeepAlive, got {:?}", conditions[1]);
        }
    } else {
        panic!("expected AllOf, got {:?}", enc.victory);
    }
}

#[test]
fn parses_nested_all_of() {
    let toml = r#"
[[encounters]]
id = "nested"
name = "Nested"
enemies = []
victory = { type = "all_of", conditions = [
    { type = "all_of", conditions = [
        { type = "all_enemies_dead" },
    ] },
    { type = "keep_alive", target_name = "VIP" },
] }
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    let enc = &encounters[0];
    if let VictoryCondition::AllOf(outer) = &enc.victory {
        assert_eq!(outer.len(), 2);
        if let VictoryCondition::AllOf(inner) = &outer[0] {
            assert_eq!(inner.len(), 1);
            assert!(matches!(inner[0], VictoryCondition::AllEnemiesDead));
        } else {
            panic!("expected nested AllOf");
        }
        if let VictoryCondition::KeepAlive { target_name, .. } = &outer[1] {
            assert_eq!(target_name, "VIP");
        } else {
            panic!("expected KeepAlive");
        }
    } else {
        panic!("expected AllOf");
    }
}

#[test]
fn all_of_with_empty_conditions_is_legal() {
    let toml = r#"
[[encounters]]
id = "empty_allof"
name = "Empty AllOf"
enemies = []
victory = { type = "all_of", conditions = [] }
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    assert!(matches!(encounters[0].victory, VictoryCondition::AllOf(ref v) if v.is_empty()));
}

// ── NPC parsing ───────────────────────────────────────────────────────────────

/// Legacy `[[encounters.npcs]]` sections in TOML are silently ignored.
/// (NPCs are now modelled as party members via `party_add` in scenario.toml.)
#[test]
fn legacy_npcs_section_in_toml_is_ignored() {
    let toml = r#"
[[encounters]]
id = "enc_with_legacy_npcs"
name = "Legacy NPC Test"
enemies = []

[[encounters.npcs]]
name = "Magister"
template = "wounded_magister"
hp_current = 4
hp_max = 6
hex_col = 6
hex_row = 4
"#;
    // Must parse without panic and produce a valid encounter (npcs field ignored).
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    assert_eq!(encounters.len(), 1);
    assert_eq!(encounters[0].enemies.len(), 0);
}

/// `keep_alive` without `target_name` must panic with a clear message.
#[test]
#[should_panic(expected = "keep_alive missing target_name")]
fn keep_alive_without_target_name_panics() {
    let toml = r#"
[[encounters]]
id = "bad"
name = "Bad"
enemies = []
victory = { type = "keep_alive" }
"#;
    load_encounters_from_str("test", "test.toml", toml, &no_templates());
}

// ── Obstacle parsing (T1.2.5) ─────────────────────────────────────────────────

/// Three obstacles parse into `EncounterDef.obstacles` with correct hex positions.
#[test]
fn parses_obstacles_section() {
    let toml = r#"
[[encounters]]
id = "enc"
name = "Enc"
enemies = []

[[encounters.obstacles]]
hex_col = 5
hex_row = 3

[[encounters.obstacles]]
hex_col = 5
hex_row = 4

[[encounters.obstacles]]
hex_col = 6
hex_row = 2
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    let obs = &encounters[0].obstacles;
    assert_eq!(obs.len(), 3);
    assert!(obs.contains(&hex_from_offset(5, 3)));
    assert!(obs.contains(&hex_from_offset(5, 4)));
    assert!(obs.contains(&hex_from_offset(6, 2)));
}

/// An encounter without an `[[obstacles]]` section parses with an empty vec.
#[test]
fn obstacles_section_optional() {
    let toml = r#"
[[encounters]]
id = "no_obs"
name = "No Obstacles"
enemies = []
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    assert!(
        encounters[0].obstacles.is_empty(),
        "obstacles must default to empty"
    );
}

/// `bootstrap_combat_state` seeds `CombatState.blocked_hexes` from the
/// `CombatBlockedHexes` resource (which is set by `spawn_combatants` from
/// `EncounterDef.obstacles`). We verify the round-trip via the Bevy App harness.
#[test]
fn bootstrap_combat_state_populates_blocked_hexes() {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;
    use storyforge::combat::bridge::{bootstrap_combat_state, CombatStateRes};
    use storyforge::game::resources::CombatBlockedHexes;

    // Use the shared bridge_app harness which registers all required resources.
    // We have to replicate it inline here because the test module cannot import
    // the `common` harness used by `tests/combat.rs`.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<storyforge::combat::bridge::UnitIdMap>()
        .init_resource::<storyforge::game::resources::HexPositions>()
        .init_resource::<storyforge::game::resources::HexCorpses>()
        .init_resource::<storyforge::game::resources::TurnQueue>()
        .init_resource::<storyforge::game::resources::CombatContext>()
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<storyforge::game::resources::CombatEnvironment>()
        .init_resource::<storyforge::game::resources::UiDirty>()
        .init_resource::<storyforge::content::content_view::ActiveContent>()
        .init_resource::<storyforge::combat::DiceRngRes>()
        .init_resource::<storyforge::game::combat_log::CombatLog>()
        .init_resource::<storyforge::ui::animation::AnimationQueue>()
        .insert_resource(storyforge::ui::hex_grid::HexGridOffset(
            bevy::math::Vec2::ZERO,
        ))
        .insert_resource(storyforge::combat::ai::world::tags::AbilityTagCache::default())
        .init_resource::<storyforge::game::resources::PresetInitiative>()
        .insert_resource(storyforge::ui::hex_grid::HexMaterials::default())
        .insert_resource(storyforge::ui::hex_grid::TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .init_resource::<storyforge::combat::bridge::BridgeQueues>()
        .init_resource::<storyforge::combat::ai::log::engine_trace::EngineTraceWriter>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<storyforge::combat::ai::log::PendingAiLogEntries>();

    // Pre-populate CombatBlockedHexes with 2 obstacle hexes.
    let hex_a = hex_from_offset(3, 2);
    let hex_b = hex_from_offset(5, 4);
    app.world_mut().resource_mut::<CombatBlockedHexes>().0 = vec![hex_a, hex_b];

    // Run bootstrap (no combatants → state.units is empty, bootstrap fills blocked_hexes).
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state should run");

    let state = app.world().resource::<CombatStateRes>();
    assert_eq!(
        state.0.blocked_hexes.len(),
        2,
        "CombatState.blocked_hexes must contain both obstacle hexes after bootstrap",
    );
    assert!(state.0.blocked_hexes.contains(&hex_a));
    assert!(state.0.blocked_hexes.contains(&hex_b));
}
