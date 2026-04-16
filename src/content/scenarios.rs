use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct ScenarioDef {
    pub id: String,
    pub name: String,
    pub party: Vec<PartyMemberDef>,
    pub scenes: Vec<SceneDef>,
}

#[derive(Debug, Clone)]
pub struct PartyMemberDef {
    pub name: String,
    pub race: String,
    pub faction: Option<String>,
    pub path: Option<String>,
    pub class_id: String,
    pub hex_pos: hexx::Hex,
}

#[derive(Debug, Clone)]
pub enum SceneDef {
    Story { text: String },
    Combat { encounter_id: String },
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ScenarioFile {
    scenarios: Vec<ScenarioRecord>,
}

#[derive(Deserialize)]
struct ScenarioRecord {
    id: String,
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
    class: String,
    hex_col: i32,
    hex_row: i32,
}

#[derive(Deserialize)]
struct SceneRecord {
    #[serde(rename = "type")]
    scene_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encounter: Option<String>,
}

const SCENARIOS_PATH: &str = "assets/data/scenarios.toml";

pub fn load_scenarios() -> Vec<ScenarioDef> {
    let src = std::fs::read_to_string(SCENARIOS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {SCENARIOS_PATH}: {e}"));
    let file: ScenarioFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {SCENARIOS_PATH}: {e}"));

    file.scenarios
        .into_iter()
        .map(|r| ScenarioDef {
            id: r.id,
            name: r.name,
            party: r
                .party
                .into_iter()
                .map(|p| PartyMemberDef {
                    name: p.name,
                    race: p.race,
                    faction: p.faction,
                    path: p.path,
                    class_id: p.class,
                    hex_pos: crate::game::hex::hex_from_offset(p.hex_col, p.hex_row),
                })
                .collect(),
            scenes: r
                .scenes
                .into_iter()
                .map(|s| match s.scene_type.as_str() {
                    "story" => SceneDef::Story {
                        text: s
                            .text
                            .unwrap_or_else(|| panic!("{SCENARIOS_PATH}: story scene missing text")),
                    },
                    "combat" => SceneDef::Combat {
                        encounter_id: s.encounter.unwrap_or_else(|| {
                            panic!("{SCENARIOS_PATH}: combat scene missing encounter")
                        }),
                    },
                    other => panic!("{SCENARIOS_PATH}: unknown scene type '{other}'"),
                })
                .collect(),
        })
        .collect()
}
