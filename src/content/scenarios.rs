use crate::content::content_view::ContentView;
use crate::content::encounters::EncounterDef;
use serde::Deserialize;
use std::collections::HashMap;

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
    /// `party_remove` to the active party. If `lines` is empty, the scene is
    /// **invisible** — advance skips past it without showing anything, which is
    /// the idiom for a pure party-change beat between visible scenes.
    Story {
        lines: Vec<DialogueLine>,
        party_add: Vec<PartyMemberDef>,
        party_remove: Vec<String>,
    },
    Combat {
        encounter_id: String,
        location: Option<String>,
        on_victory_flags: Vec<String>,
    },
    /// Player decision point. Shows `prompt` dialogue, then one button per
    /// `option`; clicking option *i* records `option[i].set_flag` into campaign
    /// flags and advances the scenario. Branching downstream is via the existing
    /// `DialogueLine.requires_flag` on later scenes.
    Choice {
        prompt: Vec<DialogueLine>,
        options: Vec<ChoiceOption>,
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
    #[serde(default)]
    options: Vec<ChoiceOptionRecord>,
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
                },
                "combat" => SceneDef::Combat {
                    encounter_id: s
                        .encounter
                        .unwrap_or_else(|| panic!("{path}: combat scene missing encounter")),
                    location: s.location,
                    on_victory_flags: s.on_victory_flags,
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
                },
                other => panic!("{path}: unknown scene type '{other}' (expected 'story', 'combat', or 'choice')"),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let SceneDef::Choice { prompt, options } = &scen.scenes[0] else {
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
        let SceneDef::Choice { prompt, options } = &scen.scenes[0] else {
            panic!("expected Choice variant");
        };
        assert!(prompt.is_empty(), "prompt should be empty when lines omitted");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].set_flag, "went");
    }
}
