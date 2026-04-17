use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct RaceDef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct FactionDef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CritFailEffect {
    #[default]
    Miss,
    ManaOverload,   // will: мана ×2, способность срабатывает
    BrokenFaith,    // faith: статус "broken_faith" — блок mana-способностей
    CircuitBreach,  // tech: самоурон = mana_cost / 2
    Exhaustion,     // heritage: статус "exhaustion" — -1 скорость, 5% hp/ход
    PactControl,    // pact: статус "pact_control" — AI управляет героем
}

#[derive(Debug, Clone)]
pub struct PathDef {
    pub id: String,
    pub name: String,
    pub crit_fail_effect: CritFailEffect,
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RaceFile {
    races: Vec<RaceRecord>,
    #[serde(default)]
    factions: Vec<FactionRecord>,
    #[serde(default)]
    paths: Vec<PathRecord>,
}

#[derive(Deserialize)]
struct RaceRecord {
    id: String,
    name: String,
    #[allow(dead_code)]
    description: String,
}

#[derive(Deserialize)]
struct FactionRecord {
    id: String,
    name: String,
    #[allow(dead_code)]
    description: String,
}

#[derive(Deserialize)]
struct PathRecord {
    id: String,
    name: String,
    #[allow(dead_code)]
    description: String,
    #[serde(default)]
    crit_fail_effect: Option<CritFailRecord>,
}

#[derive(Deserialize)]
struct CritFailRecord {
    #[serde(rename = "type")]
    effect_type: String,
}

pub const RACES_FILE: &str = "races.toml";

pub fn load_races() -> (Vec<RaceDef>, Vec<FactionDef>, Vec<PathDef>) {
    let path = format!("assets/data/{RACES_FILE}");
    if !std::path::Path::new(&path).is_file() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_races(&path, &src)
}

pub fn parse_races(path: &str, src: &str) -> (Vec<RaceDef>, Vec<FactionDef>, Vec<PathDef>) {
    let file: RaceFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));

    let races = file
        .races
        .into_iter()
        .map(|r| RaceDef {
            id: r.id,
            name: r.name,
        })
        .collect();

    let factions = file
        .factions
        .into_iter()
        .map(|f| FactionDef {
            id: f.id,
            name: f.name,
        })
        .collect();

    let paths = file
        .paths
        .into_iter()
        .map(|p| {
            let crit_fail_effect = match &p.crit_fail_effect {
                None => CritFailEffect::Miss,
                Some(r) => match r.effect_type.as_str() {
                    "mana_overload" => CritFailEffect::ManaOverload,
                    "broken_faith" => CritFailEffect::BrokenFaith,
                    "circuit_breach" => CritFailEffect::CircuitBreach,
                    "exhaustion" => CritFailEffect::Exhaustion,
                    "pact_control" => CritFailEffect::PactControl,
                    other => panic!("{path}: unknown crit_fail_effect type '{other}'"),
                },
            };
            PathDef {
                id: p.id,
                name: p.name,
                crit_fail_effect,
            }
        })
        .collect();

    (races, factions, paths)
}
