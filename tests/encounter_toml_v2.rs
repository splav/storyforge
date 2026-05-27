//! Tests for T1.1.4: TOML parsing for `keep_alive`, `all_of` and `[[encounters.npcs]]`.

use storyforge::content::encounters::{load_encounters_from_str, VictoryCondition};
use std::collections::HashMap;

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

#[test]
fn parses_npcs_section_with_hex_pos() {
    let toml = r#"
[[encounters]]
id = "enc_with_npcs"
name = "NPC Test"
enemies = []

[[encounters.npcs]]
name = "Magister"
template = "wounded_magister"
hp_current = 4
hp_max = 6
hex_col = 6
hex_row = 4
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    let enc = &encounters[0];
    assert_eq!(enc.npcs.len(), 1);
    let npc = &enc.npcs[0];
    assert_eq!(npc.name, "Magister");
    assert_eq!(npc.template, "wounded_magister");
    assert_eq!(npc.hp_current, 4);
    assert_eq!(npc.hp_max, 6);
    assert_eq!(npc.hex_col, 6);
    assert_eq!(npc.hex_row, 4);
}

#[test]
fn npcs_default_empty_when_section_omitted() {
    let toml = r#"
[[encounters]]
id = "no_npcs"
name = "No NPCs"
enemies = []
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    assert!(encounters[0].npcs.is_empty());
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

/// `hp_current` defaults to `hp_max` when omitted.
#[test]
fn npc_hp_current_defaults_to_hp_max() {
    let toml = r#"
[[encounters]]
id = "enc"
name = "Enc"
enemies = []

[[encounters.npcs]]
name = "Guard"
template = "guard_npc"
hp_max = 10
hex_col = 1
hex_row = 1
"#;
    let encounters = load_encounters_from_str("test", "test.toml", toml, &no_templates());
    let npc = &encounters[0].npcs[0];
    assert_eq!(npc.hp_current, 10, "hp_current should default to hp_max");
    assert_eq!(npc.hp_max, 10);
}
