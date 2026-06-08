use crate::content::content_view::ContentView;
use crate::content::encounters::EncounterDef;
use serde::Deserialize;
use std::collections::HashMap;

/// One operation in a story scene's ordered `status_ops` list: apply or remove a
/// persistent status on a named party member. Operations fold in declaration order
/// across scenes (see `active_party_statuses`), so a single ordered list fully and
/// deterministically describes the result — `add X` then later `remove X` then
/// `add Y` all compose unambiguously.
#[derive(Debug, Clone)]
pub enum PartyStatusOp {
    /// Apply `status_id` to `unit_name` (`unit_name` must match a `PartyMemberDef::name`;
    /// `status_id` must exist in `content.statuses`).
    Add { unit_name: String, status_id: String },
    /// Remove `status_id` from `unit_name` (no-op if the unit does not currently carry it).
    Remove { unit_name: String, status_id: String },
}

impl PartyStatusOp {
    /// Affected party member's name, regardless of op kind.
    pub fn unit_name(&self) -> &str {
        match self {
            PartyStatusOp::Add { unit_name, .. } | PartyStatusOp::Remove { unit_name, .. } => unit_name,
        }
    }

    /// Status id involved, regardless of op kind.
    pub fn status_id(&self) -> &str {
        match self {
            PartyStatusOp::Add { status_id, .. } | PartyStatusOp::Remove { status_id, .. } => status_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScenarioDef {
    pub id: String,
    pub name: String,
    pub party: Vec<PartyMemberDef>,
    pub scenes: Vec<SceneDef>,
    /// Fully-merged rules (global → campaign → scenario layers). Single source
    /// of content lookups during this scenario's combat.
    pub content: ContentView,
    /// Encounters referenced by this scenario's `Scene::Combat` scenes.
    /// Filled by the campaign loader after parsing; scoped to this scenario only.
    pub encounters: HashMap<String, EncounterDef>,
}

#[derive(Debug, Clone)]
pub struct PartyMemberDef {
    pub name: String,
    pub race: String,
    pub faction: Option<String>,
    pub path: Option<String>,
    /// Class-based member: resolved via `content.classes`.
    pub class_id: String,
    pub hex_pos: hexx::Hex,
    /// Template-based member: resolved via `content.unit_templates`.
    /// When set, `class_id` is ignored by `spawn_combatants`.
    pub template: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SceneDef {
    /// Dialogue scene. On advance (after the last line) applies `party_add` /
    /// `party_remove` to the active party, and the ordered `status_ops` (persistent
    /// status add/remove on named members). If `lines` is empty, the scene is
    /// **invisible** — advance skips past it without showing anything, which is the
    /// idiom for a pure party-change beat between visible scenes.
    Story {
        lines: Vec<DialogueLine>,
        party_add: Vec<PartyMemberDef>,
        party_remove: Vec<String>,
        /// Ordered persistent-status operations on named party members, applied in
        /// declaration order when folded (see `active_party_statuses`).
        status_ops: Vec<PartyStatusOp>,
        /// If `Some(f)`, this scene is skipped when `f` is absent from
        /// `CampaignState.flags`. Composes with `is_invisible`: either reason
        /// causes the scene to be skipped.
        requires_flag: Option<String>,
    },
    Combat {
        encounter_id: String,
        location: Option<String>,
        on_victory_flags: Vec<String>,
        /// If `Some(f)`, this scene is skipped when `f` is absent from
        /// `CampaignState.flags`. NOTE: skipping a `Combat` scene discards its
        /// `on_victory_flags` — any downstream-needed flag must be set by the
        /// branching `Choice` option or a story scene instead.
        requires_flag: Option<String>,
    },
    /// Player decision point. Shows `prompt` dialogue, then one button per
    /// `option`; clicking option *i* records `option[i].set_flag` into campaign
    /// flags and advances the scenario. Branching downstream is via the existing
    /// `DialogueLine.requires_flag` on later scenes.
    Choice {
        prompt: Vec<DialogueLine>,
        options: Vec<ChoiceOption>,
        /// If `Some(f)`, this scene is skipped when `f` is absent from
        /// `CampaignState.flags`.
        requires_flag: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ChoiceOption {
    pub label: String,
    pub set_flag: String,
}

impl SceneDef {
    /// True if this scene has no visible representation — advance_scenario
    /// should auto-skip past it.
    pub fn is_invisible(&self) -> bool {
        matches!(self, SceneDef::Story { lines, .. } if lines.is_empty())
    }

    /// Returns the scene-level gate flag, if any. When `Some(f)` and `f` is
    /// absent from `CampaignState.flags`, the scene is skipped exactly like an
    /// invisible scene.
    pub fn requires_flag(&self) -> Option<&str> {
        match self {
            SceneDef::Story { requires_flag, .. } => requires_flag.as_deref(),
            SceneDef::Combat { requires_flag, .. } => requires_flag.as_deref(),
            SceneDef::Choice { requires_flag, .. } => requires_flag.as_deref(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DialogueLine {
    pub speaker: String,
    pub text: String,
    pub requires_flag: Option<String>,
}

/// Party active when entering scene at `up_to` (i.e. after effects of all prior scenes).
pub fn active_party(scen: &ScenarioDef, up_to: usize) -> Vec<PartyMemberDef> {
    let mut party = scen.party.clone();
    for scene in scen.scenes.iter().take(up_to) {
        if let SceneDef::Story { party_add, party_remove, .. } = scene {
            if !party_remove.is_empty() {
                party.retain(|m| !party_remove.iter().any(|n| n == &m.name));
            }
            for m in party_add {
                party.push(m.clone());
            }
        }
    }
    party
}

/// Accumulated persistent statuses for the active party entering scene at `up_to`.
///
/// Folds every story scene's `status_ops` in **declaration order** across scenes
/// `0..up_to`: `Add` inserts (deduplicated per unit), `Remove` deletes. Because a
/// single ordered list drives the result, the fold is fully deterministic and
/// order-significant (`add X … remove X … add Y` composes exactly as written).
/// Returns `unit_name → Vec<status_id>`; empty entries are dropped so callers can
/// cheaply check `.contains_key`.
pub fn active_party_statuses(
    scen: &ScenarioDef,
    up_to: usize,
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for scene in scen.scenes.iter().take(up_to) {
        if let SceneDef::Story { status_ops, .. } = scene {
            for op in status_ops {
                match op {
                    PartyStatusOp::Add { unit_name, status_id } => {
                        let list = map.entry(unit_name.clone()).or_default();
                        if !list.contains(status_id) {
                            list.push(status_id.clone());
                        }
                    }
                    PartyStatusOp::Remove { unit_name, status_id } => {
                        if let Some(list) = map.get_mut(unit_name) {
                            list.retain(|s| s != status_id);
                        }
                    }
                }
            }
        }
    }

    map.retain(|_, v| !v.is_empty());
    map
}



// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ScenarioRecord {
    name: String,
    party: Vec<PartyRecord>,
    scenes: Vec<SceneRecord>,
}


#[derive(Deserialize)]
struct PartyRecord {
    name: String,
    race: String,
    #[serde(default)]
    faction: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    class: String,
    hex_col: i32,
    hex_row: i32,
    /// Optional template id (unit_templates.toml). When set, `class` is ignored.
    #[serde(default)]
    template: Option<String>,
}

#[derive(Deserialize)]
struct PartyStatusOpRecord {
    /// `"add"` | `"remove"`.
    op: String,
    unit_name: String,
    status_id: String,
}

#[derive(Deserialize)]
struct SceneRecord {
    #[serde(rename = "type")]
    scene_type: String,
    #[serde(default)]
    lines: Option<Vec<DialogueLineRecord>>,
    #[serde(default)]
    encounter: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    on_victory_flags: Vec<String>,
    /// Members to append to the active party on scene advance (story scenes only).
    #[serde(default)]
    party_add: Vec<PartyRecord>,
    /// Names of members to drop from the party on scene advance (story scenes only).
    #[serde(default)]
    party_remove: Vec<String>,
    /// Ordered persistent-status operations on named party members (story scenes).
    #[serde(default)]
    status_ops: Vec<PartyStatusOpRecord>,
    #[serde(default)]
    options: Vec<ChoiceOptionRecord>,
    /// Scene-level flag gate. If present, the scene is skipped when the named flag
    /// is absent from `CampaignState.flags`.
    #[serde(default)]
    requires_flag: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceOptionRecord {
    label: String,
    set_flag: String,
}

#[derive(Deserialize)]
struct DialogueLineRecord {
    speaker: String,
    text: String,
    #[serde(default)]
    requires_flag: Option<String>,
}

fn convert_party_record(p: PartyRecord) -> PartyMemberDef {
    PartyMemberDef {
        name: p.name,
        race: p.race,
        faction: p.faction,
        path: p.path,
        class_id: p.class,
        hex_pos: crate::game::hex::hex_from_offset(p.hex_col, p.hex_row),
        template: p.template,
    }
}

/// Parses a single `scenario.toml` body. `id` is supplied by the caller (folder
/// name); `path` is only used for error messages. Returned `ScenarioDef` has
/// `encounters` empty — the campaign loader fills it in after reading the
/// sibling `encounters.toml`.
pub fn parse_scenario_body(id: &str, path: &str, src: &str) -> ScenarioDef {
    let r: ScenarioRecord =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));

    ScenarioDef {
        id: id.to_string(),
        name: r.name,
        party: r.party.into_iter().map(convert_party_record).collect(),
        // Populated by the campaign loader via ContentView::load_layered.
        content: ContentView::default(),
        encounters: HashMap::new(),
        scenes: r
            .scenes
            .into_iter()
            .map(|s| match s.scene_type.as_str() {
                                "story" => SceneDef::Story {
                    // `lines = []` is legal and produces an invisible scene (pure
                    // party-change beat). Missing `lines` key is also treated as empty.
                    lines: s
                        .lines
                        .unwrap_or_default()
                        .into_iter()
                        .map(|l| DialogueLine {
                            speaker: l.speaker,
                            text: l.text,
                            requires_flag: l.requires_flag,
                        })
                        .collect(),
                    party_add: s
                        .party_add
                        .into_iter()
                        .map(convert_party_record)
                        .collect(),
                    party_remove: s.party_remove,
                    status_ops: s
                        .status_ops
                        .into_iter()
                        .map(|r| match r.op.as_str() {
                            "add" => PartyStatusOp::Add { unit_name: r.unit_name, status_id: r.status_id },
                            "remove" => PartyStatusOp::Remove { unit_name: r.unit_name, status_id: r.status_id },
                            other => panic!(
                                "{path}: story scene status op has unknown op '{other}' (expected 'add' or 'remove')"
                            ),
                        })
                        .collect(),
                    requires_flag: s.requires_flag.clone(),
                },
                                "combat" => SceneDef::Combat {
                    encounter_id: s
                        .encounter
                        .unwrap_or_else(|| panic!("{path}: combat scene missing encounter")),
                    location: s.location,
                    on_victory_flags: s.on_victory_flags,
                    requires_flag: s.requires_flag.clone(),
                },
                                "choice" => SceneDef::Choice {
                    prompt: s.lines.unwrap_or_default().into_iter()
                        .map(|l| DialogueLine {
                            speaker: l.speaker,
                            text: l.text,
                            requires_flag: l.requires_flag,
                        })
                        .collect(),
                    options: s.options.into_iter()
                        .map(|o| ChoiceOption { label: o.label, set_flag: o.set_flag })
                        .collect(),
                    requires_flag: s.requires_flag.clone(),
                },
                other => panic!("{path}: unknown scene type '{other}' (expected 'story', 'combat', or 'choice')"),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────────

    fn make_scenario_with_statuses() -> ScenarioDef {
        // party: Alice (scene 0), Bob added at scene 0 advance
        // scene 0: story — ops: add Alice/injured, add Bob/injured, remove Bob/injured
        // scene 1: story — ops: add Alice/exhaustion
        let party = vec![PartyMemberDef {
            name: "Alice".into(),
            race: "human".into(),
            faction: None,
            path: None,
            class_id: "warrior".into(),
            hex_pos: hexx::Hex::ZERO,
            template: None,
        }];
        let bob = PartyMemberDef {
            name: "Bob".into(),
            race: "human".into(),
            faction: None,
            path: None,
            class_id: "mage".into(),
            hex_pos: hexx::Hex::ZERO,
            template: None,
        };
        ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party,
            scenes: vec![
                SceneDef::Story {
                    lines: vec![],
                    party_add: vec![bob],
                    party_remove: vec![],
                    // Ordered: add injured to Alice and Bob, then remove it from Bob.
                    status_ops: vec![
                        PartyStatusOp::Add { unit_name: "Alice".into(), status_id: "injured".into() },
                        PartyStatusOp::Add { unit_name: "Bob".into(),   status_id: "injured".into() },
                        PartyStatusOp::Remove { unit_name: "Bob".into(), status_id: "injured".into() },
                    ],
                    requires_flag: None,
                },
                SceneDef::Story {
                    lines: vec![],
                    party_add: vec![],
                    party_remove: vec![],
                    status_ops: vec![
                        PartyStatusOp::Add { unit_name: "Alice".into(), status_id: "exhaustion".into() },
                    ],
                    requires_flag: None,
                },
            ],
            content: ContentView::default(),
            encounters: HashMap::new(),
        }
    }

    // ── active_party_statuses tests ──────────────────────────────────────────────

    /// Before any scenes are processed (up_to=0) the status map is empty.
    #[test]
    fn fold_empty_before_any_scene() {
        let scen = make_scenario_with_statuses();
        let map = active_party_statuses(&scen, 0);
        assert!(map.is_empty(), "no statuses before any scene");
    }

    /// After scene 0: Alice has [injured], Bob has [] (remove undoes add).
    #[test]
    fn fold_add_and_remove_within_same_scene() {
        let scen = make_scenario_with_statuses();
        let map = active_party_statuses(&scen, 1);
        assert_eq!(map.get("Alice"), Some(&vec!["injured".to_string()]));
        // Bob: injured was added then removed — should not appear at all.
        assert!(!map.contains_key("Bob"), "Bob's injured was removed");
    }

    /// After scene 1: Alice has [injured, exhaustion].
    #[test]
    fn fold_accumulates_across_scenes() {
        let scen = make_scenario_with_statuses();
        let map = active_party_statuses(&scen, 2);
        let alice = map.get("Alice").expect("Alice should have statuses");
        assert_eq!(alice, &vec!["injured".to_string(), "exhaustion".to_string()]);
    }

    /// Idempotency: adding the same status twice results in it appearing only once.
    #[test]
    fn fold_dedup_same_status_added_twice() {
        let scen = ScenarioDef {
            id: "s".into(),
            name: "s".into(),
            party: vec![PartyMemberDef {
                name: "Alice".into(),
                race: "human".into(),
                faction: None,
                path: None,
                class_id: "warrior".into(),
                hex_pos: hexx::Hex::ZERO,
                template: None,
            }],
            scenes: vec![
                SceneDef::Story {
                    lines: vec![],
                    party_add: vec![],
                    party_remove: vec![],
                    status_ops: vec![
                        PartyStatusOp::Add { unit_name: "Alice".into(), status_id: "injured".into() },
                    ],
                    requires_flag: None,
                },
                SceneDef::Story {
                    lines: vec![],
                    party_add: vec![],
                    party_remove: vec![],
                    // Add injured again — should be deduplicated.
                    status_ops: vec![
                        PartyStatusOp::Add { unit_name: "Alice".into(), status_id: "injured".into() },
                    ],
                    requires_flag: None,
                },
            ],
            content: ContentView::default(),
            encounters: HashMap::new(),
        };
        let map = active_party_statuses(&scen, 2);
        let alice = map.get("Alice").expect("Alice should have statuses");
        assert_eq!(alice.len(), 1, "injured must appear only once after two adds");
    }

    /// Remove of a status that was never added is a no-op (no panic, no phantom entry).
    #[test]
    fn fold_remove_nonexistent_is_noop() {
        let scen = ScenarioDef {
            id: "s".into(),
            name: "s".into(),
            party: vec![],
            scenes: vec![SceneDef::Story {
                lines: vec![],
                party_add: vec![],
                party_remove: vec![],
                status_ops: vec![
                    PartyStatusOp::Remove { unit_name: "Ghost".into(), status_id: "injured".into() },
                ],
                requires_flag: None,
            }],
            content: ContentView::default(),
            encounters: HashMap::new(),
        };
        let map = active_party_statuses(&scen, 1);
        assert!(map.is_empty(), "removing a never-added status leaves empty map");
    }

    // ── Parsing tests ────────────────────────────────────────────────────────────

    /// An ordered `status_ops` list (add/remove) in a story scene parses, preserving
    /// op kind and declaration order.
    #[test]
    fn parse_story_with_status_ops() {
        let toml = r#"
name = "test"
party = []

[[scenes]]
type = "story"

[[scenes.status_ops]]
op = "add"
unit_name = "Alice"
status_id = "injured"

[[scenes.status_ops]]
op = "remove"
unit_name = "Bob"
status_id = "exhaustion"
"#;
        let scen = parse_scenario_body("s1", "test.toml", toml);
        let SceneDef::Story { status_ops, .. } = &scen.scenes[0] else {
            panic!("expected Story");
        };
        assert_eq!(status_ops.len(), 2);
        assert!(matches!(&status_ops[0],
            PartyStatusOp::Add { unit_name, status_id } if unit_name == "Alice" && status_id == "injured"));
        assert!(matches!(&status_ops[1],
            PartyStatusOp::Remove { unit_name, status_id } if unit_name == "Bob" && status_id == "exhaustion"));
    }

    /// A story scene with no `status_ops` key parses with an empty vec.
    #[test]
    fn parse_story_missing_status_fields_defaults_to_empty() {
        let toml = r#"
name = "test"
party = []

[[scenes]]
type = "story"
"#;
        let scen = parse_scenario_body("s1", "test.toml", toml);
        let SceneDef::Story { status_ops, .. } = &scen.scenes[0] else {
            panic!("expected Story");
        };
        assert!(status_ops.is_empty());
    }

    /// An unknown `op` value in a status op panics at parse time.
    #[test]
    #[should_panic(expected = "unknown op 'toggle'")]
    fn parse_story_status_op_unknown_op_panics() {
        let toml = r#"
name = "test"
party = []

[[scenes]]
type = "story"

[[scenes.status_ops]]
op = "toggle"
unit_name = "Alice"
status_id = "injured"
"#;
        parse_scenario_body("s1", "test.toml", toml);
    }

    /// A `type = "choice"` scene with two options and a prompt line parses into
    /// `SceneDef::Choice` with the correct prompt and options.
    #[test]
    fn parse_choice_scene_with_prompt_and_options() {
        let toml = r#"
name = "test"
party = []

[[scenes]]
type = "choice"

[[scenes.lines]]
speaker = "Narrator"
text = "What do you do?"

[[scenes.options]]
label = "Help"
set_flag = "helped"

[[scenes.options]]
label = "Ignore"
set_flag = "ignored"
"#;
        let scen = parse_scenario_body("s1", "test.toml", toml);
        assert_eq!(scen.scenes.len(), 1);
        let SceneDef::Choice { prompt, options, .. } = &scen.scenes[0] else {
            panic!("expected Choice variant");
        };
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0].speaker, "Narrator");
        assert_eq!(prompt[0].text, "What do you do?");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "Help");
        assert_eq!(options[0].set_flag, "helped");
        assert_eq!(options[1].label, "Ignore");
        assert_eq!(options[1].set_flag, "ignored");
    }

    /// A `type = "choice"` scene with `lines` omitted → empty prompt, options
    /// still parsed correctly.
    #[test]
    fn parse_choice_scene_omitted_lines_gives_empty_prompt() {
        let toml = r#"
name = "test"
party = []

[[scenes]]
type = "choice"

[[scenes.options]]
label = "Go"
set_flag = "went"
"#;
        let scen = parse_scenario_body("s1", "test.toml", toml);
        let SceneDef::Choice { prompt, options, .. } = &scen.scenes[0] else {
            panic!("expected Choice variant");
        };
        assert!(prompt.is_empty(), "prompt should be empty when lines omitted");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].set_flag, "went");
    }
}
