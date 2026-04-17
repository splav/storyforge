use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CampaignDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub scenario_ids: Vec<String>,
}

#[derive(Deserialize)]
struct CampaignFile {
    campaigns: Vec<CampaignRecord>,
}

#[derive(Deserialize)]
struct CampaignRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    scenarios: Vec<String>,
}

const CAMPAIGNS_PATH: &str = "assets/data/campaigns.toml";

pub fn load_campaigns() -> Vec<CampaignDef> {
    let src = std::fs::read_to_string(CAMPAIGNS_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {CAMPAIGNS_PATH}: {e}"));
    let file: CampaignFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {CAMPAIGNS_PATH}: {e}"));

    file.campaigns
        .into_iter()
        .map(|r| CampaignDef {
            id: r.id,
            name: r.name,
            description: r.description,
            scenario_ids: r.scenarios,
        })
        .collect()
}
