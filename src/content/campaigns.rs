//! Campaign + scenario + encounter loader.
//!
//! Disk layout (every content file is optional at every layer):
//!
//! ```text
//! assets/data/                        # global layer (rules)
//!   abilities.toml / statuses.toml / classes.toml / unit_templates.toml
//!   races.toml / equipment/{weapons,chest,legs,feet}.toml / settings.toml
//!   campaigns/
//!     <campaign_id>/                  # campaign layer (folder name = id)
//!       campaign.toml                 # metadata (name, scenarios = [...] order)
//!       (any content file, optional — overrides global)
//!       <scenario_id>/                # scenario layer (folder name = id)
//!         scenario.toml               # metadata: name, party, scenes
//!         encounters.toml             # scenario-owned encounters
//!         (any content file, optional — overrides campaign + global)
//! ```
//!
//! For each scenario we build a fully-merged `ContentView` (global ∪ campaign ∪
//! scenario, with scenario winning on id clash) and attach it as
//! `ScenarioDef.content`. At scenario entry the runtime copies this into
//! `ActiveContent`, which is the single source of content for combat systems.

use crate::content::content_view::ContentView;
use crate::content::encounters::load_encounters_from_str;
use crate::content::scenarios::{parse_scenario_body, ScenarioDef};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct CampaignDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub scenario_ids: Vec<String>,
}

#[derive(Deserialize)]
struct CampaignRecord {
    name: String,
    #[serde(default)]
    description: String,
    scenarios: Vec<String>,
}

const CAMPAIGNS_DIR: &str = "assets/data/campaigns";

pub struct CampaignsLoad {
    pub campaigns: Vec<CampaignDef>,
    pub scenarios: HashMap<String, ScenarioDef>,
}

pub fn load_campaigns() -> CampaignsLoad {
    let campaigns_dir = Path::new(CAMPAIGNS_DIR);
    let entries = std::fs::read_dir(campaigns_dir)
        .unwrap_or_else(|e| panic!("Cannot read {CAMPAIGNS_DIR}: {e}"));

    let mut campaign_ids: Vec<String> = entries
        .filter_map(|r| r.ok())
        .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    campaign_ids.sort();

    let mut campaigns = Vec::with_capacity(campaign_ids.len());
    let mut scenarios = HashMap::new();

    for campaign_id in campaign_ids {
        let campaign_dir = campaigns_dir.join(&campaign_id);
        let campaign_file = campaign_dir.join("campaign.toml");
        let src = std::fs::read_to_string(&campaign_file).unwrap_or_else(|e| {
            panic!("Cannot read {}: {e}", campaign_file.display())
        });
        let rec: CampaignRecord = toml::from_str(&src).unwrap_or_else(|e| {
            panic!("Cannot parse {}: {e}", campaign_file.display())
        });

        for scenario_id in &rec.scenarios {
            let scen_dir = campaign_dir.join(scenario_id);
            let scen_file = scen_dir.join("scenario.toml");
            let enc_file = scen_dir.join("encounters.toml");

            let scen_src = std::fs::read_to_string(&scen_file).unwrap_or_else(|e| {
                panic!("Cannot read {}: {e}", scen_file.display())
            });
            let mut scen = parse_scenario_body(
                scenario_id,
                &scen_file.display().to_string(),
                &scen_src,
            );

            // Layered content view: global → campaign → scenario.
            scen.content = ContentView::load_layered(&campaign_dir, &scen_dir);

            // Encounters resolve templates against the scenario's merged pool.
            let enc_src = std::fs::read_to_string(&enc_file).unwrap_or_else(|e| {
                panic!("Cannot read {}: {e}", enc_file.display())
            });
            let encounters = load_encounters_from_str(
                scenario_id,
                &enc_file.display().to_string(),
                &enc_src,
                &scen.content.unit_templates,
            );
            scen.encounters = encounters
                .into_iter()
                .map(|e| (e.id.clone(), e))
                .collect();

            let prev = scenarios.insert(scenario_id.clone(), scen);
            assert!(
                prev.is_none(),
                "duplicate scenario id '{scenario_id}' across campaigns",
            );
        }

        campaigns.push(CampaignDef {
            id: campaign_id,
            name: rec.name,
            description: rec.description,
            scenario_ids: rec.scenarios,
        });
    }

    CampaignsLoad { campaigns, scenarios }
}
