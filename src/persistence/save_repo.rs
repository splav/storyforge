use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{fs, io};

use crate::persistence::paths::AppPaths;

pub const SLOT_COUNT: u8 = 3;

/// Versioned on-disk format. Adding a new variant + migration keeps old saves readable.
#[derive(Serialize, Deserialize)]
#[serde(tag = "version")]
enum SaveSlotFile {
    #[serde(rename = "1")]
    V1(SlotProfileV1),
}

/// A slot is a user profile: one save per campaign + the last campaign played.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct SlotProfileV1 {
    pub last_campaign: Option<String>,
    pub campaigns: HashMap<String, CampaignProgress>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CampaignProgress {
    pub scenario_index: usize,
    pub scenario_id: String,
    pub scene_index: usize,
    pub saved_at: u64,
}

fn slot_path(paths: &AppPaths, slot: u8) -> std::path::PathBuf {
    paths.saves_dir().join(format!("slot_{slot}.toml"))
}

pub fn exists(paths: &AppPaths, slot: u8) -> bool {
    slot_path(paths, slot).is_file()
}

pub fn load(paths: &AppPaths, slot: u8) -> Option<SlotProfileV1> {
    let path = slot_path(paths, slot);
    if !path.is_file() {
        return None;
    }
    match fs::read_to_string(&path) {
        Ok(src) => match toml::from_str::<SaveSlotFile>(&src) {
            Ok(SaveSlotFile::V1(v1)) => {
                info!(
                    "loaded slot {slot} from {} (campaigns={}, last={:?})",
                    path.display(),
                    v1.campaigns.len(),
                    v1.last_campaign,
                );
                Some(v1)
            }
            Err(e) => {
                let bak = path.with_extension("toml.bak");
                if let Err(re) = fs::rename(&path, &bak) {
                    warn!("slot {slot} parse failed ({e}); backup rename also failed: {re}");
                } else {
                    warn!(
                        "slot {slot} parse failed ({e}); backed up to {}",
                        bak.display()
                    );
                }
                None
            }
        },
        Err(e) => {
            warn!("cannot read slot {slot}: {e}");
            None
        }
    }
}

pub fn save(paths: &AppPaths, slot: u8, data: &SlotProfileV1) -> io::Result<()> {
    fs::create_dir_all(paths.saves_dir())?;
    let file = SaveSlotFile::V1(data.clone());
    let text = toml::to_string_pretty(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = slot_path(paths, slot);
    fs::write(&path, text)?;
    info!(
        "saved slot {slot} to {} (campaigns={}, last={:?})",
        path.display(),
        data.campaigns.len(),
        data.last_campaign,
    );
    Ok(())
}

pub fn delete(paths: &AppPaths, slot: u8) -> io::Result<()> {
    let path = slot_path(paths, slot);
    if path.exists() {
        fs::remove_file(&path)?;
        info!("deleted slot {slot} at {}", path.display());
    }
    Ok(())
}

/// Write a campaign's current position into the slot, updating `last_campaign`.
/// Reads existing profile to preserve other campaigns' progress.
pub fn record_progress(
    paths: &AppPaths,
    slot: u8,
    campaign_id: &str,
    scenario_index: usize,
    scenario_id: &str,
    scene_index: usize,
) -> io::Result<()> {
    let mut profile = load(paths, slot).unwrap_or_default();
    profile.campaigns.insert(
        campaign_id.to_string(),
        CampaignProgress {
            scenario_index,
            scenario_id: scenario_id.to_string(),
            scene_index,
            saved_at: now_unix(),
        },
    );
    profile.last_campaign = Some(campaign_id.to_string());
    save(paths, slot, &profile)
}

/// Drop a campaign's record from the slot (on completion or explicit delete).
/// Clears `last_campaign` if it pointed to this one.
pub fn clear_campaign(paths: &AppPaths, slot: u8, campaign_id: &str) -> io::Result<()> {
    let Some(mut profile) = load(paths, slot) else { return Ok(()) };
    profile.campaigns.remove(campaign_id);
    if profile.last_campaign.as_deref() == Some(campaign_id) {
        profile.last_campaign = None;
    }
    if profile.campaigns.is_empty() && profile.last_campaign.is_none() {
        delete(paths, slot)
    } else {
        save(paths, slot, &profile)
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_data() {
        let mut campaigns = HashMap::new();
        campaigns.insert(
            "camp_a".to_string(),
            CampaignProgress {
                scenario_index: 2,
                scenario_id: "scen_x".into(),
                scene_index: 3,
                saved_at: 1_700_000_000,
            },
        );
        let original = SlotProfileV1 {
            last_campaign: Some("camp_a".into()),
            campaigns,
        };
        let text = toml::to_string_pretty(&SaveSlotFile::V1(original.clone())).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        assert_eq!(parsed.last_campaign, original.last_campaign);
        assert_eq!(parsed.campaigns.len(), 1);
        let pr = parsed.campaigns.get("camp_a").unwrap();
        assert_eq!(pr.scenario_index, 2);
        assert_eq!(pr.scenario_id, "scen_x");
        assert_eq!(pr.scene_index, 3);
        assert_eq!(pr.saved_at, 1_700_000_000);
    }

    #[test]
    fn empty_profile_roundtrip() {
        let original = SlotProfileV1::default();
        let text = toml::to_string_pretty(&SaveSlotFile::V1(original)).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        assert!(parsed.last_campaign.is_none());
        assert!(parsed.campaigns.is_empty());
    }
}
